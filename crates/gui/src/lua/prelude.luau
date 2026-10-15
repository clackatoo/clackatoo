-- The client's API, as config sees it, over the runtime's core functions.
--
-- Windows, workspaces, tabs and panes are handles: tables holding an ID,
-- whose fields are looked up in the client's latest picture of the world
-- each time they're read. A handle to something that's gone raises an
-- error when read. Two handles to the same thing are equal.
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

local function handle(class, id)
	if id == nil then
		return nil
	end
	return setmetatable({ id = id }, class)
end

local function handles(class, ids)
	local list = {}
	for index, id in ipairs(ids) do
		list[index] = handle(class, id)
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
		return subscription(call(core.on, kind, self.id, event, handler))
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
		local info = core.info(kind, self.id)
		if info == nil then
			error(`{kind} {self.id} has closed`, 2)
		end
		return field(info)
	end
	class.__eq = function(a, b)
		return a.id == b.id
	end
	class.__tostring = function(self)
		return `{kind} {self.id}`
	end
end

define(Window, "window", {
	workspace = function(info)
		return handle(Workspace, info.workspace)
	end,
	tab = function(info)
		return handle(Tab, info.tab)
	end,
	pane = function(info)
		return handle(Pane, info.pane)
	end,
}, {})

define(Workspace, "workspace", {
	name = function(info)
		return info.name
	end,
	tabs = function(info)
		return handles(Tab, info.tabs)
	end,
}, {})

define(Tab, "tab", {
	name = function(info)
		return info.name
	end,
	panes = function(info)
		return handles(Pane, info.panes)
	end,
	focused = function(info)
		return handle(Pane, info.focused)
	end,
	workspace = function(info)
		return handle(Workspace, info.workspace)
	end,
}, {})

define(Pane, "pane", {
	kind = function(info)
		return info.kind
	end,
	title = function(info)
		return info.title
	end,
	tab = function(info)
		return handle(Tab, info.tab)
	end,
}, {
	split = function(self, side, options)
		local kind = if options and options.kind then options.kind else self.kind
		local id, err = coroutine.yield(REQUEST, call(core.split, self.id, side, kind))
		if err ~= nil then
			error(err, 2)
		end
		return handle(Pane, id)
	end,
	close = function(self)
		call(core.close, self.id)
	end,
	focus = function(self)
		call(core.focus, self.id)
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
		map = function(modes, keys, handler)
			call(core.map, modes, keys, handler)
		end,
		unmap = function(modes, keys)
			call(core.unmap, modes, keys)
		end,
	}),
	on = function(event, handler)
		return subscription(call(core.on, "client", 0, event, handler))
	end,
	-- Runs a command, as if typed after `:`, as Neovim's `vim.cmd`:
	-- `client.cmd("Files")`.
	cmd = function(line)
		call(core.command, line)
	end,
	-- Forwards a port from this machine to the daemon's, when the window
	-- is on another machine's, as `:PortForward` does:
	-- `client.forward(8080)`, `client.forward("3000:8080")`.
	forward = function(spec)
		call(core.forward, tostring(spec))
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
			return handles(Window, core.windows())
		elseif key == "focused" then
			return handle(Pane, core.focused())
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
			window = handle(Window, info.window),
			pane = handle(Pane, info.pane),
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
}
