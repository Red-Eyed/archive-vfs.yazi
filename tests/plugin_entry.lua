local events = {
	commands = {},
	debug = {},
	emits = {},
	notifications = {},
}

local command_status = nil
local command_error = "helper executable not found"

ya = {
	sync = function(fn)
		return fn
	end,
	dbg = function(...)
		table.insert(events.debug, { ... })
	end,
	emit = function(...)
		table.insert(events.emits, { ... })
	end,
	notify = function(notification)
		table.insert(events.notifications, notification)
	end,
}

Error = {
	fs = function(error)
		return error
	end,
}

local zip_url = setmetatable({ path = "/fixtures/example.zip", ext = "zip" }, {
	__tostring = function(url)
		return "regular://" .. url.path
	end,
})

cx = {
	active = {
		current = {
			hovered = {
				url = zip_url,
				cha = { is_dir = false },
			},
		},
	},
}

Command = function(name)
	local command = { name = name }

	function command:arg(args)
		self.args = args
		return self
	end

	function command:status()
		table.insert(events.commands, self)
		return command_status, command_error
	end

	return command
end

Url = function(value)
	return value
end

local function reset_events()
	for _, values in pairs(events) do
		for index = #values, 1, -1 do
			table.remove(values, index)
		end
	end
end

local function assert_equal(actual, expected, message)
	if actual ~= expected then
		error(string.format("%s: expected %s, got %s", message, tostring(expected), tostring(actual)))
	end
end

local plugin = dofile("main.lua")

plugin:entry({ args = {} })
assert_equal(#events.debug > 0, true, "entry invocation is logged")
assert_equal(#events.notifications, 1, "missing helper is reported")
assert_equal(events.notifications[1].level, "error", "missing helper is an error")

reset_events()
command_status = { success = false }
command_error = nil
plugin:entry({ args = {} })
assert_equal(#events.notifications, 1, "rejected ZIP is reported")
assert_equal(events.notifications[1].level, "warn", "rejected ZIP is a warning")

reset_events()
command_status = { success = true }
plugin:entry({ args = {} })
assert_equal(events.emits[1][1], "cd", "recognized ZIP enters its mount")
assert_equal(events.emits[1][2][1], "archive://local//fixtures/example.zip", "mount URL uses the archive path")

reset_events()
local returned_url = plugin:provide({ op = "Absolute", url = zip_url })
assert_equal(returned_url, zip_url, "provider contract is unchanged")
assert_equal(#events.debug, 1, "provider invocation is logged")

print("plugin entry integration tests passed")
