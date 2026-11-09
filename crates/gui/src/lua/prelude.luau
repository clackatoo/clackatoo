-- The client's API, as config sees it, over the runtime's core functions.
--
-- Windows, workspaces, tabs and panes are handles: tables holding an ID,
-- and the window they were found through, whose fields are looked up in
-- that window's latest picture of the world each time they're read. Panes
-- are numbered by their daemon, and two windows can be on two daemons, so
-- a pane is always a window's. A handle to something that's gone raises
-- an error when read. Two handles to the same thing in the same window are
-- equal.
--
-- Waiting for the daemon, as `pane:split` does, yields the handler's
-- coroutine with a request; the runtime resumes it with the answer.

local core = ...

-- Yielded first, with a request's number, by whatever waits for the
-- daemon, so the runtime knows the pause for one.
local REQUEST = table.freeze({})

-- Calls a core function from one of the API's, raising what goes wrong
-- as a mistake where the API's function was called.
local function call(f, ...)
	local results = table.pack(pcall(f, ...))
	if not results[1] then
		local message = string.match(tostring(results[2]), "^[^\n]*")
		error((string.gsub(message, "^runtime error: ", "")), 3)
	end
	return table.unpack(results, 2, results.n)
end

local Window, Workspace, Tab, Pane = {}, {}, {}, {}

local function handle(class, window, id)
	if id == nil then
		return nil
	end
	return setmetatable({ id = id, window = window }, class)
end

local function handles(class, window, ids)
	local list = {}
	for index, id in ipairs(ids) do
		list[index] = handle(class, window, id)
	end
	return list
end

local Subscription = {}
Subscription.__index = {
	off = function(self)
		call(core.off, self.id)
	end,
}

local function subscription(id)
	return setmetatable({ id = id }, Subscription)
end

-- Makes `class` the handles of `kind`, with `fields` read from its info
-- and `methods` of its own.
local function define(class, kind, fields, methods)
	methods.on = function(self, event, handler)
		return subscription(call(core.on, kind, self.window, self.id, event, handler))
	end
	class.__index = function(self, key)
		local method = methods[key]
		if method ~= nil then
			return method
		end
		local field = fields[key]
		if field == nil then
			return nil
		end
		local info = core.info(kind, self.window, self.id)
		if info == nil then
			error(`{kind} {self.id} has closed`, 2)
		end
		return field(self.window, info)
	end
	class.__eq = function(a, b)
		return a.id == b.id and a.window == b.window
	end
	class.__tostring = function(self)
		return `{kind} {self.id}`
	end
end

define(Window, "window", {
	workspace = function(window, info)
		return handle(Workspace, window, info.workspace)
	end,
	tab = function(window, info)
		return handle(Tab, window, info.tab)
	end,
	pane = function(window, info)
		return handle(Pane, window, info.pane)
	end,
}, {})

define(Workspace, "workspace", {
	name = function(_, info)
		return info.name
	end,
	tabs = function(window, info)
		return handles(Tab, window, info.tabs)
	end,
}, {})

define(Tab, "tab", {
	name = function(_, info)
		return info.name
	end,
	panes = function(window, info)
		return handles(Pane, window, info.panes)
	end,
	focused = function(window, info)
		return handle(Pane, window, info.focused)
	end,
	workspace = function(window, info)
		return handle(Workspace, window, info.workspace)
	end,
}, {})

define(Pane, "pane", {
	kind = function(_, info)
		return info.kind
	end,
	title = function(_, info)
		return info.title
	end,
	tab = function(window, info)
		return handle(Tab, window, info.tab)
	end,
}, {
	split = function(self, side, options)
		local kind = if options and options.kind then options.kind else self.kind
		local id, err = coroutine.yield(REQUEST, call(core.split, self.window, self.id, side, kind))
		if err ~= nil then
			error(err, 2)
		end
		return handle(Pane, self.window, id)
	end,
	close = function(self)
		call(core.close, self.window, self.id)
	end,
	focus = function(self)
		call(core.focus, self.window, self.id)
	end,
})

-- An event's state, kept off the event itself.
local stopped = setmetatable({}, { __mode = "k" })
local decided = setmetatable({}, { __mode = "k" })

local Event = {}
Event.__index = {
	stop = function(self)
		if decided[self] then
			error("too late to stop the event: stop before waiting on anything", 2)
		end
		stopped[self] = true
	end,
	propagate = function(self)
		if decided[self] then
			error("too late to let the event go on: propagate before waiting on anything", 2)
		end
		stopped[self] = false
	end,
}

local client = {
	keys = table.freeze({
		-- Binds keys to a function, or to keys to type, with options as
		-- Neovim's `vim.keymap.set` takes them: `{desc = "…"}` says what
		-- the binding does, in the panel of keys. `{propagate = true}` lets
		-- its keys go on at once, without waiting for the function, which
		-- runs after.
		map = function(modes, keys, handler, options)
			call(core.map, modes, keys, handler, options)
		end,
		unmap = function(modes, keys)
			call(core.unmap, modes, keys)
		end,
		-- Names the bindings some keys start, in the panel of keys, as
		-- which-key.nvim's `group` does: `client.keys.group("n",
		-- "<Space>g", "git")`. No name stops naming them. Keys bound to
		-- something of their own say what that does instead.
		group = function(modes, keys, name)
			call(core.group, modes, keys, name)
		end,
	}),
	on = function(event, handler)
		return subscription(call(core.on, "client", 0, 0, event, handler))
	end,
	-- Runs a command, as if typed after `:`, as Neovim's `vim.cmd`:
	-- `client.cmd("Files")`.
	cmd = function(line)
		call(core.command, line)
	end,
	-- Forwards a port from this machine to the daemon's, in each window on
	-- another machine's, as `:PortForward` does, now and as windows open:
	-- `client.forward(8080)`, `client.forward("3000:8080")`.
	forward = function(spec)
		call(core.forward, tostring(spec))
	end,
	-- Has the daemon bring another machine's workspaces in, as
	-- `:connect HOST` does, saying nothing unless it fails:
	-- `client.connect("desk")`. Each window asks its daemon, now and as
	-- windows open, and again as each connects; the daemon keeps its
	-- connections, so asking again leaves one as it is.
	-- `client.connect("server", {share = false})` keeps it from the
	-- daemons connected to this one, as `:connect HOST --no-share` does.
	connect = function(host, options)
		call(core.connect, tostring(host), options)
	end,
	-- Ends the daemon's connection to another machine, as
	-- `:disconnect HOST` does: `client.disconnect("desk")`.
	disconnect = function(host)
		call(core.disconnect, tostring(host))
	end,
	-- The options, by name or short name, as Vim's `vim.opt`:
	-- `client.opt.number = false`.
	opt = setmetatable({}, {
		__index = function(_, name)
			return call(core.get_option, name)
		end,
		__newindex = function(_, name, value)
			call(core.set_option, name, value)
		end,
	}),
}
setmetatable(client, {
	__index = function(_, key)
		if key == "windows" then
			local list = {}
			for index, window in ipairs(core.windows()) do
				list[index] = handle(Window, window, window)
			end
			return list
		elseif key == "focused" then
			local window, pane = core.focused()
			return handle(Pane, window, pane)
		end
		return nil
	end,
})

return {
	client = client,
	request = REQUEST,
	-- An event from what the window says about it, stopped to begin with
	-- or not.
	event = function(info, stop)
		local event = setmetatable({
			key = info.key,
			keys = info.keys,
			text = info.text,
			mode = info.mode,
			window = handle(Window, info.window, info.window),
			pane = handle(Pane, info.window, info.pane),
			-- The count typed before a binding's keys, 0 if none, as
			-- Vim's `v:count`; and 1 if none, as `v:count1`.
			count = info.count or 0,
			count1 = info.count or 1,
		}, Event)
		stopped[event] = stop
		return event
	end,
	-- Whether `event` is stopped so far.
	stopped = function(event)
		return stopped[event] == true
	end,
	-- Whether `event` was stopped, which from now on stays as it is.
	decide = function(event)
		decided[event] = true
		return stopped[event] == true
	end,
	-- A chat's turn having ended, from what the window that heard it says.
	turnend = function(info)
		return table.freeze({
			chat = info.chat,
			title = info.title,
			agent = info.agent,
			failed = info.failed,
			window = handle(Window, info.window, info.window),
			pane = handle(Pane, info.window, info.pane),
		})
	end,
}
