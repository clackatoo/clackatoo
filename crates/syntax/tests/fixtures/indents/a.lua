local function f(a)
if a then
return 1
elseif a == 2 then
return 2
else
local t = {
x = 1,
y = 2,
}
end
for i = 1, 3 do
print(i)
end
end

return {
f = f,
}
