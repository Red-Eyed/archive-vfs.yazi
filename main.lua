--- @since 26.9.1

local M = {}

local LIST_MAGIC = "AVFSL1\0\0"
local STAT_MAGIC = "AVFSS1\0\0"
local PATH_MAGIC = "AVFSP1\0\0"
local FILE_MODE = tonumber("100444", 8)
local DIRECTORY_MODE = tonumber("40555", 8)

local IMAGE_MIMES = {
	avif = "image/avif",
	bmp = "image/bmp",
	gif = "image/gif",
	heic = "image/heic",
	heif = "image/heif",
	jpeg = "image/jpeg",
	jpg = "image/jpeg",
	jxl = "image/jxl",
	png = "image/png",
	svg = "image/svg+xml",
	tif = "image/tiff",
	tiff = "image/tiff",
	webp = "image/webp",
}

local TEXT_EXTENSIONS = {
	bash = true,
	c = true,
	cc = true,
	cfg = true,
	conf = true,
	cpp = true,
	css = true,
	csv = true,
	go = true,
	h = true,
	hpp = true,
	html = true,
	ini = true,
	java = true,
	js = true,
	jsx = true,
	lua = true,
	md = true,
	py = true,
	rs = true,
	sh = true,
	sql = true,
	toml = true,
	ts = true,
	tsx = true,
	txt = true,
	xml = true,
	yaml = true,
	yml = true,
	zsh = true,
}

local function fs_error(kind, message)
	return Error.fs { kind = kind, message = message }
end

local function helper()
	return os.getenv("ARCHIVE_VFS_HELPER") or "archive-vfs-helper"
end

local function run_output(args)
	local output, err = Command(helper()):arg(args):output()
	if not output then
		return nil, err
	end
	if not output.status.success then
		local message = #output.stderr > 0 and output.stderr or "archive-vfs-helper failed"
		return nil, fs_error("Other", message)
	end
	return output.stdout
end

local function spawn_reader(args, stdin)
	return Command(helper())
		:arg(args)
		:stdin(stdin and Command.PIPED or Command.NULL)
		:stdout(Command.PIPED)
		:stderr(Command.INHERIT)
		:spawn()
end

local function bytes_to_string(bytes)
	if type(bytes) == "string" then
		return bytes
	end
	local chunks = {}
	for first = 1, #bytes, 4096 do
		local last = math.min(first + 4095, #bytes)
		chunks[#chunks + 1] = string.char(table.unpack(bytes, first, last))
	end
	return table.concat(chunks)
end

local function read_exact(child, length)
	local chunks, remaining = {}, length
	while remaining > 0 do
		local chunk, event = child:read(remaining)
		if event ~= 0 or not chunk or #chunk == 0 then
			return nil, fs_error("UnexpectedEof", "archive-vfs-helper ended mid-record")
		end
		chunks[#chunks + 1] = bytes_to_string(chunk)
		remaining = remaining - #chunk
	end
	return table.concat(chunks)
end

local function unpack_exact(child, format, length)
	local bytes, err = read_exact(child, length)
	if not bytes then
		return nil, err
	end
	return string.unpack(format, bytes)
end

local function finish_reader(child)
	local status, err = child:wait()
	if status and status.success then
		return true
	end
	return false, err or fs_error("Other", "archive-vfs-helper failed")
end

local function archive_parts(url)
	local base = url.base or url
	local inner = url:strip_prefix(base)
	return tostring(base.path), inner and tostring(inner) or ""
end

local function optional_inner_args(command, archive, inner)
	local args = { command, archive }
	if #inner > 0 then
		args[#args + 1] = inner
	end
	return args
end

local function identity(child)
	local size, err = unpack_exact(child, "<I8", 8)
	if not size then
		return nil, err
	end
	local modified_ns, modified_err = unpack_exact(child, "<I8", 8)
	if not modified_ns then
		return nil, modified_err
	end
	local tag, tag_err = unpack_exact(child, "<I8", 8)
	if not tag then
		return nil, tag_err
	end
	return { size = size, modified_ns = modified_ns, tag = tag }
end

local function read_node(child)
	local kind, kind_err = unpack_exact(child, "<I1", 1)
	if not kind then
		return nil, kind_err
	end
	local size, size_err = unpack_exact(child, "<I8", 8)
	if not size then
		return nil, size_err
	end
	local modified, modified_err = unpack_exact(child, "<i8", 8)
	if not modified then
		return nil, modified_err
	end
	local name_len, name_err = unpack_exact(child, "<I4", 4)
	if not name_len then
		return nil, name_err
	end
	local name, read_err = read_exact(child, name_len)
	if not name then
		return nil, read_err
	end
	return { kind = kind, size = size, modified = modified, name = name }
end

local function cha(node, archive)
	local modified = node.modified >= 0 and node.modified or archive.modified_ns / 1000000000
	return Cha {
		mode = node.kind == 1 and DIRECTORY_MODE or FILE_MODE,
		len = node.size,
		mtime = modified,
		ctime = archive.modified_ns / 1000000000,
		dev = archive.tag,
	}
end

local function list(url)
	local archive, inner = archive_parts(url)
	local child, spawn_err = spawn_reader(optional_inner_args("list", archive, inner), false)
	if not child then
		return nil, spawn_err
	end
	local magic, magic_err = read_exact(child, #LIST_MAGIC)
	if not magic then
		return nil, magic_err
	end
	if magic ~= LIST_MAGIC then
		return nil, fs_error("InvalidData", "archive-vfs-helper returned an incompatible list protocol")
	end
	local count, count_err = unpack_exact(child, "<I8", 8)
	if not count then
		return nil, count_err
	end
	local archive_identity, identity_err = identity(child)
	if not archive_identity then
		return nil, identity_err
	end
	local entries = {}
	for index = 1, count do
		local node, node_err = read_node(child)
		if not node then
			return nil, node_err
		end
		local attributes = cha(node, archive_identity)
		entries[index] = {
			cha = attributes,
			file = File { url = url:join(node.name), cha = attributes },
		}
	end
	local ok, finish_err = finish_reader(child)
	return ok and entries or nil, finish_err
end

local function stat(url)
	local archive, inner = archive_parts(url)
	local child, spawn_err = spawn_reader(optional_inner_args("stat", archive, inner), false)
	if not child then
		return nil, spawn_err
	end
	local magic, magic_err = read_exact(child, #STAT_MAGIC)
	if not magic then
		return nil, magic_err
	end
	if magic ~= STAT_MAGIC then
		return nil, fs_error("InvalidData", "archive-vfs-helper returned an incompatible stat protocol")
	end
	local archive_identity, identity_err = identity(child)
	if not archive_identity then
		return nil, identity_err
	end
	local node, node_err = read_node(child)
	if not node then
		return nil, node_err
	end
	local ok, finish_err = finish_reader(child)
	return ok and cha(node, archive_identity) or nil, finish_err
end

local function file(url)
	local attributes, err = stat(url)
	if not attributes then
		return nil, err
	end
	return File { url = url, cha = attributes }
end

local function mime_for(file)
	if file.cha.is_dir then
		return "folder/local"
	end
	local extension = file.url.ext and file.url.ext:lower() or ""
	if IMAGE_MIMES[extension] then
		return IMAGE_MIMES[extension]
	end
	if extension == "json" then
		return "application/json"
	end
	if extension == "ndjson" or extension == "jsonl" then
		return "application/ndjson"
	end
	if TEXT_EXTENSIONS[extension] then
		return "text/plain"
	end
	return "application/octet-stream"
end

local function lease(file)
	local archive, inner = archive_parts(file.url)
	local child, spawn_err = spawn_reader({ "lease", archive, inner }, true)
	if not child then
		return nil, spawn_err
	end
	local magic, magic_err = read_exact(child, #PATH_MAGIC)
	if not magic then
		return nil, magic_err
	end
	if magic ~= PATH_MAGIC then
		return nil, fs_error("InvalidData", "archive-vfs-helper returned an incompatible lease protocol")
	end
	local length, length_err = unpack_exact(child, "<I4", 4)
	if not length then
		return nil, length_err
	end
	local path, path_err = read_exact(child, length)
	if not path then
		return nil, path_err
	end
	return child, Path.os(path)
end

local function release(child)
	ya.drop(child:take_stdin())
	return finish_reader(child)
end

local function with_backing(job, operation)
	local child, path_or_err = lease(job.file)
	if not child then
		return nil, path_or_err
	end
	local backed = File { url = job.file.url, cha = job.file.cha, backing = path_or_err }
	local ok, result = pcall(operation, ya.dict_merge(job, { file = backed }))
	local released, release_err = release(child)
	if not ok then
		error(result)
	end
	if not released then
		return nil, release_err
	end
	return result
end

local function previewer(mime)
	if mime:find("^image/") then
		return require("image")
	end
	if mime == "application/json" or mime == "application/ndjson" then
		return require("json")
	end
	if mime:find("^text/") then
		return require("code")
	end
	return require("file")
end

local function readonly()
	return false, fs_error("PermissionDenied", "archive-vfs is read-only")
end

local function unsupported()
	return nil, fs_error("Unsupported", "archive-vfs does not expose archive symlinks")
end

local function set_local_attrs(path, attributes)
	if attributes.mode then
		local mode = string.format("%o", attributes.mode % 4096)
		local status, err = Command("chmod"):arg({ mode, tostring(path) }):status()
		if not status or not status.success then
			return false, err or fs_error("Other", "chmod failed for copied archive member")
		end
	end
	if attributes.mtime then
		local status, err = Command("touch")
			:arg({ "-m", "-t", os.date("%Y%m%d%H%M.%S", math.floor(attributes.mtime)), tostring(path) })
			:status()
		if not status or not status.success then
			return false, err or fs_error("Other", "touch failed for copied archive member")
		end
	end
	return true
end

function M:Capabilities()
	return { copy_progressive = true }
end

function M:ReadDir(job)
	return list(job.url)
end

function M:File(job)
	return file(job.url)
end

function M:Revalidate(job)
	local latest, err = file(job.file.url)
	if not latest then
		return nil, err
	end
	local old, new = job.file.cha, latest.cha
	if old.dev == new.dev and old.len == new.len and old.mtime == new.mtime and old.mode == new.mode then
		return nil
	end
	return latest
end

function M:SymlinkMetadata(job)
	return stat(job.url)
end

function M:Metadata(job)
	return stat(job.url)
end

function M:Canonicalize(job)
	return job.url
end

function M:Absolute(job)
	return job.url
end


function M:Casefold(job)
	return job.url
end

function M:Open(job)
	local demand = job.demand
	if demand.append or demand.create or demand.create_new or demand.truncate or demand.write then
		return readonly()
	end
	return 0
end

function M:Read(job)
	local archive, inner = archive_parts(job.url)
	return run_output({ "read", archive, inner, "--offset", tostring(job.offset), "--len", tostring(job.len) })
end

function M:Copy(job)
	local archive, inner = archive_parts(job.from)
	local attributes, stat_err = stat(job.from)
	if not attributes then
		return nil, stat_err
	end
	local status, err = Command(helper()):arg({ "copy", archive, inner, tostring(job.to) }):status()
	if status and status.success then
		local set, set_err = set_local_attrs(job.to, job.attrs)
		return set and attributes.len or nil, set_err
	end
	return nil, err or fs_error("Other", "archive-vfs-helper copy failed")
end

function M:CopyProgressive(job)
	local archive, inner = archive_parts(job.from)
	local child, spawn_err = spawn_reader({ "stream", archive, inner }, false)
	if not child then
		return false, spawn_err
	end
	local destination, open_err = io.open(tostring(job.to), "wb")
	if not destination then
		child:start_kill()
		return false, fs_error("Other", open_err or "cannot open copy destination")
	end
	while true do
		local chunk, event = child:read(1024 * 1024)
		if event ~= 0 or not chunk or #chunk == 0 then
			break
		end
		chunk = bytes_to_string(chunk)
		local wrote, write_err = destination:write(chunk)
		if not wrote then
			destination:close()
			child:start_kill()
			return false, fs_error("Other", write_err or "cannot write copy destination")
		end
		local sent, send_err = job.tx:send(#chunk)
		if not sent then
			destination:close()
			child:start_kill()
			return false, send_err
		end
	end
	destination:close()
	local finished, finish_err = finish_reader(child)
	if not finished then
		return false, finish_err
	end
	return set_local_attrs(job.to, job.attrs)
end

M.CreateDir = readonly
M.HardLink = readonly
M.ReadLink = unsupported
M.RemoveDir = readonly
M.RemoveDirAll = readonly
M.RemoveFile = readonly
M.Rename = readonly
M.SetAttrs = readonly
M.SetLen = readonly
M.Symlink = readonly
M.Trash = readonly
M.Write = readonly

function M:provide(job)
	local operation = self[job.op]
	if not operation then
		return readonly()
	end
	return operation(self, job)
end

function M:fetch(job)
	return ya.co(function()
		local updates = {}
		for _, candidate in ipairs(job.files) do
			local mime = mime_for(candidate)
			if coroutine.yield(candidate, { mime }) then
				updates[candidate.url] = mime
			end
		end
		if next(updates) then
			ya.emit("update_mimes", { updates = updates })
		end
	end)
end

function M:peek(job)
	return with_backing(job, function(backed_job)
		return previewer(job.mime):peek(backed_job)
	end)
end

function M:seek(job)
	return previewer(job.mime):seek(job)
end

function M:preload(job)
	if not job.mime:find("^image/") then
		return true
	end
	return with_backing(job, function(backed_job)
		return require("image"):preload(backed_job)
	end)
end

local hovered = ya.sync(function()
	local file = cx.active.current.hovered
	if not file then
		return nil
	end
	return {
		url = file.url,
		is_dir = file.cha.is_dir,
		is_mount = tostring(file.url):find("^archive://") ~= nil,
	}
end)

local selected_or_hovered = ya.sync(function()
	local tab, urls = cx.active, {}
	for _, candidate in pairs(tab.selected) do
		urls[#urls + 1] = candidate.url
	end
	if #urls == 0 and tab.current.hovered then
		urls[1] = tab.current.hovered.url
	end
	return urls
end)

local function materialize_url(url)
	if not tostring(url):find("^archive://") then
		return url
	end
	local plain, file_err = file(url)
	if not plain then
		return nil, file_err
	end
	if plain.cha.is_dir then
		return url
	end
	local child, path_or_err = lease(plain)
	if not child then
		return nil, path_or_err
	end
	local released, release_err = release(child)
	if not released then
		return nil, release_err
	end
	return Url(path_or_err)
end

local function open_selected()
	local targets = {}
	for _, url in ipairs(selected_or_hovered()) do
		local target, err = materialize_url(url)
		if not target then
			ya.notify { title = "archive-vfs", content = tostring(err), level = "error", timeout = 5 }
			return
		end
		targets[#targets + 1] = target
	end
	if #targets > 0 then
		ya.emit("escape", { visual = true })
		ya.emit("open", targets)
	end
end

function M:entry(job)
	if job.args[1] == "open" then
		return open_selected()
	end
	local file = hovered()
	if not file then
		return
	end
	if file.is_dir then
		ya.emit("enter", {})
		return
	end
	if file.is_mount then
		return
	end
	local status = Command(helper()):arg({ "probe", tostring(file.url.path) }):status()
	if status and status.success then
		ya.emit("cd", { Url("archive://local/" .. tostring(file.url.path)) })
	end
end

return M
