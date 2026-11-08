-- Real insert-completion regression. Run after `cargo build`:
--   MATHPREVIEW_CLI=target/debug/mathpreview-cli \
--     nvim --headless -u NONE -i NONE -l tests/nvim/completion_sync.lua
-- A child Neovim runs its normal input loop while this driver waits for the
-- daemon. Calling :doautocmd would miss the popup-specific event suppression.

local repo = vim.fn.getcwd()
local cli = vim.fn.fnamemodify(vim.env.MATHPREVIEW_CLI
  or (repo .. "/target/debug/mathpreview-cli"), ":p")
assert(vim.fn.executable(cli) == 1, "build mathpreview-cli before running this test")
local scratch = vim.fn.tempname() .. "-mathpreview-completion-sync"
assert(vim.fn.mkdir(scratch, "p") == 1, "could not create scratch directory")
scratch = assert((vim.uv or vim.loop).fs_realpath(scratch), "could not canonicalize scratch directory")
local child = vim.fn.jobstart({ vim.v.progpath, "--embed", "--headless", "-u", "NONE", "-i", "NONE" }, {
  rpc = true,
  env = { XDG_CONFIG_HOME = scratch .. "/config", XDG_STATE_HOME = scratch .. "/state" },
})
assert(child > 0, "could not start child Neovim")

local function lua(source, ...)
  return vim.rpcrequest(child, "nvim_exec_lua", source, { ... })
end

local function input(keys)
  local encoded = vim.api.nvim_replace_termcodes(keys, true, false, true)
  assert(vim.rpcrequest(child, "nvim_input", encoded) == #encoded, "child rejected input")
end

local function wait_for(predicate, message)
  if vim.wait(5000, predicate, 25) then return end
  error(message .. "\nchild: " .. vim.inspect(lua([[
    return { mode = vim.fn.mode(), popup = vim.fn.pumvisible(),
      line = vim.api.nvim_get_current_line(),
      events = vim.list_slice(events, math.max(1, #events - 7)),
      posts = vim.list_slice(posts, math.max(1, #posts - 3)), status = mp.status() }
  ]])))
end

local function request(port, route)
  local res = vim.system({ "curl", "--silent", "--show-error", "--fail-with-body",
    "--max-time", "1", "http://127.0.0.1:" .. tostring(port) .. (route or "/") }, { text = true }):wait()
  return res.code == 0 and res.stdout or ""
end

local function page(port)
  local paragraphs = {}
  for paragraph in request(port):gmatch("<p[^>]*>(.-)</p>") do
    paragraphs[#paragraphs + 1] = paragraph:gsub("<[^>]*>", ""):gsub("%s+$", "")
  end
  return table.concat(paragraphs, "\n") .. "\n"
end

local function pause()
  vim.wait(150, function() return false end, 10)
end

local ok, err = xpcall(function()
  lua([[
    local repo, cli, baseline = ...
    vim.opt.runtimepath:prepend(repo)
    vim.o.hidden = true
    vim.o.completeopt = "menu,menuone,noselect,noinsert"
    mp = baseline and baseline ~= "" and dofile(baseline) or require("mathpreview")
    mp.setup({ cmd = cli, auto_open_browser = false, stale_check = false,
      sync = true, raise_on_jump = false, close_on_exit = true,
      debounce_ms = 40, cursor_debounce_ms = 80,
      jump_wait_ms = 1000, jump_retry_ms = 10 })
    events, posts, jobs = {}, {}, {}
    local system = vim.system
    vim.system = function(args, opts, done)
      local url = args[#args]
      if type(url) == "string" and (url:sub(-7) == "/buffer" or url:sub(-7) == "/cursor"
          or url:sub(-13) == "/editor-state") then
        posts[#posts + 1] = { args = args, body = opts.stdin, url = url,
          cursor = vim.api.nvim_win_get_cursor(0), popup = vim.fn.pumvisible(),
          edit_cursor = vim.api.nvim_buf_get_mark(0, "."),
          stream_sent = stream and stream.sent or nil }
      end
      return system(args, opts, done)
    end
    local jobstart = vim.fn.jobstart
    vim.fn.jobstart = function(args, opts)
      local job = jobstart(args, opts)
      if type(args) == "table" and args[1] == cli and args[2] == "serve" and job > 0 then
        jobs[#jobs + 1] = job
      end
      return job
    end
    vim.api.nvim_create_autocmd({ "TextChangedI", "TextChangedP", "CursorMovedI" }, {
      callback = function(a)
        events[#events + 1] = { name = a.event, popup = vim.fn.pumvisible() }
      end,
    })
  ]], repo, cli, vim.env.MATHPREVIEW_BASELINE_LUA or "")

  for _, format in ipairs({ "markdown", "tex" }) do
    local path = scratch .. "/completion." .. (format == "markdown" and "md" or "tex")
    local prefix = "Completion probe "
    local lines = format == "markdown" and { "# Completion test", "", prefix }
      or { [[\documentclass{article}]], [[\begin{document}]], prefix, [[\end{document}]] }
    assert(vim.fn.writefile(lines, path) == 0, "could not write fixture")
    lua([[
      local path, format = ...
      vim.cmd("edit " .. vim.fn.fnameescape(path))
      vim.bo.filetype = format
      vim.api.nvim_win_set_cursor(0, { 3, 0 })
      mp.start()
    ]], path, format)
    wait_for(function()
      return lua([[return mp.status().daemon_root == ... and mp.status().daemon_port ~= nil]], path)
    end, "daemon did not start for " .. format)
    local port = lua("return mp.status().daemon_port")
    wait_for(function() return page(port):find("Completion probe", 1, true) ~= nil end,
      "initial page did not render for " .. format)
    input("A")
    wait_for(function() return lua([[return vim.fn.mode():sub(1, 1) == "i"]]) end,
      "could not enter Insert mode")
    pause()
    lua([[vim.fn.complete(..., { "alpha", "alphabet", "alpine" })]], #prefix + 1)
    wait_for(function() return lua("return vim.fn.pumvisible() == 1") end,
      "completion popup did not open")
    pause()

    local function assert_live(keys, suffix, label)
      lua("events, posts = {}, {}")
      input(keys)
      local expected = prefix .. suffix
      wait_for(function()
        return lua([[return vim.fn.pumvisible() == 1 and vim.api.nvim_get_current_line() == ...]], expected)
      end, label .. " did not keep the completion popup open")
      wait_for(function() return page(port):find(expected .. "\n", 1, true) ~= nil end,
        label .. " did not update the preview while completion was open")
      wait_for(function()
        return lua([[
          local col = ...
          for _, post in ipairs(posts) do
            if post.url:sub(-7) == "/cursor" then
              local payload = vim.json.decode(post.body)
              if payload.line == 3 and payload.col == col and payload.typing == true then return true end
            end
          end
          return false
        ]], #expected + 1)
      end, label .. " did not follow the typing cursor while completion was open")
      local snapshot = lua("return { events = events, posts = posts, popup = vim.fn.pumvisible() }")
      assert(snapshot.popup == 1, label .. " closed the popup before the assertions")
      local saw_popup_change, saw_buffer = false, false
      for _, event in ipairs(snapshot.events) do
        if event.name == "TextChangedP" then saw_popup_change = true end
        assert(event.name ~= "TextChangedI" and event.name ~= "CursorMovedI",
          label .. " unexpectedly used ordinary Insert-mode events")
      end
      assert(saw_popup_change, label .. " did not produce a real TextChangedP event")
      for _, post in ipairs(snapshot.posts) do
        if post.url:sub(-7) == "/buffer" and post.body:find(expected, 1, true) then
          saw_buffer = true
          local edit_header = ("X-Mathpreview-Cursor: %d:%d"):format(
            post.edit_cursor[1], post.edit_cursor[2] + 1)
          assert(vim.tbl_contains(post.args, edit_header),
            label .. " lost the most recent edit position in its buffer snapshot")
        end
      end
      assert(saw_buffer, label .. " did not push the unsaved buffer")
    end

    assert_live("a", "a", format .. " first letter")
    assert_live("l", "al", format .. " second letter")
    assert_live("<C-n>", "alpha", format .. " completion selection")
    input("<C-e>")
    wait_for(function()
      return lua([[return vim.fn.pumvisible() == 0 and vim.api.nvim_get_current_line() == ...]], prefix .. "al")
        and page(port):find(prefix .. "al\n", 1, true) ~= nil
    end, format .. " completion cancellation did not restore the typed text")

    -- A completion engine can draw its own popup, leaving TextChangedI in
    -- use. Also cover uninterrupted input faster than either configured
    -- interval: updates must occur during the burst, not only after silence.
    pause()
    lua([[
      mp.setup({ debounce_ms = 120, cursor_debounce_ms = 120 })
      events, posts = {}, {}
      stream = { sent = 0, total = 60, finished = false }
      local timer = (vim.uv or vim.loop).new_timer()
      timer:start(10, 10, vim.schedule_wrap(function()
        stream.sent = stream.sent + 1
        vim.api.nvim_input("z")
        if stream.sent == stream.total then
          stream.finished = true
          timer:stop()
          timer:close()
        end
      end))
    ]])
    wait_for(function()
      return lua([[
        local buffer, cursor = false, false
        for _, post in ipairs(posts) do
          if post.stream_sent and post.stream_sent < stream.total then
            if post.url:sub(-7) == "/buffer" then buffer = true end
            if post.url:sub(-7) == "/cursor" then cursor = true end
          end
        end
        return buffer and cursor and not stream.finished
      ]]) and page(port):find(prefix .. "alz", 1, true) ~= nil
    end, format .. " continuous typing waited for the end of the burst")
    local final_text = prefix .. "al" .. string.rep("z", 60)
    wait_for(function()
      return lua("return stream.finished")
        and page(port):find(final_text .. "\n", 1, true) ~= nil
        and lua([[
          for i = #posts, 1, -1 do
            if posts[i].url:sub(-7) == "/cursor" then
              local cursor = vim.json.decode(posts[i].body)
              return cursor.line == 3 and cursor.col == ... and cursor.typing == true
            end
          end
          return false
        ]], #final_text + 1)
    end, format .. " continuous typing lost its final buffer or cursor")
    local stream_posts = lua("return posts")
    local buffer_posts, cursor_posts = 0, 0
    for _, post in ipairs(stream_posts) do
      if post.url:sub(-7) == "/buffer" then buffer_posts = buffer_posts + 1 end
      if post.url:sub(-7) == "/cursor" then cursor_posts = cursor_posts + 1 end
    end
    assert(buffer_posts < 20 and cursor_posts < 20,
      format .. " stopped coalescing fast keystrokes")
    lua("mp.setup({ debounce_ms = 40, cursor_debounce_ms = 80 }); stream = nil")
    assert(vim.deep_equal(vim.fn.readfile(path), lines), "preview synchronization saved the source file")
    input("<Esc>")
    wait_for(function() return lua("return vim.fn.mode() == 'n'") end, "could not leave Insert mode")
    lua("mp.stop()")
  end

  -- Faster scheduling must still leave an included preamble frozen until
  -- focus returns to the document, including popup edits and invalid saves.
  local root, preamble = scratch .. "/preamble-main.tex", scratch .. "/preamble.tex"
  local initial_preamble = {
    [[\newcommand{\PreviewA}{InitialAlpha}]],
    [[\newcommand{\PreviewB}{InitialBeta}]],
  }
  assert(vim.fn.writefile(initial_preamble, preamble) == 0)
  assert(vim.fn.writefile({ [[\documentclass{article}]], [[\input{preamble}]],
    [[\begin{document}]], [[Preamble probe $\PreviewA + \PreviewB$.]],
    [[\end{document}]] }, root) == 0)
  lua([[
    vim.cmd("edit " .. vim.fn.fnameescape(...))
    vim.bo.filetype = "tex"
    root_buf = vim.api.nvim_get_current_buf()
    mp.start()
  ]], root)
  wait_for(function()
    return lua([[return mp.status().daemon_root == ... and mp.status().daemon_port ~= nil]], root)
  end, "preamble fixture daemon did not start")
  local port = lua("return mp.status().daemon_port")
  wait_for(function()
    local raw = request(port)
    return raw:find("InitialAlpha", 1, true) and raw:find("InitialBeta", 1, true)
  end, "initial preamble definitions did not render")
  local baseline = request(port)
  lua([[
    vim.cmd("edit " .. vim.fn.fnameescape(...))
    vim.bo.filetype = "tex"
    preamble_buf = vim.api.nvim_get_current_buf()
  ]], preamble)
  wait_for(function()
    local ok_debug, state = pcall(vim.json.decode, request(port, "/debug"))
    return ok_debug and state.preamble_edit_paused == true
  end, "entering the included preamble did not hold rendering")
  lua("events, posts = {}, {}")

  local function expect_frozen(ms, label, expected_page)
    local deadline = (vim.uv or vim.loop).hrtime() + ms * 1e6
    repeat
      assert(request(port) == (expected_page or baseline), label .. " changed the rendered page")
      for _, post in ipairs(lua("return posts")) do
        assert(not (post.url:sub(-7) == "/buffer"
          and vim.tbl_contains(post.args, "X-Mathpreview-Path: " .. preamble)),
          label .. " pushed a preamble through the live buffer route")
      end
      vim.wait(25, function() return false end, 5)
    until (vim.uv or vim.loop).hrtime() >= deadline
  end

  lua([[vim.api.nvim_win_set_cursor(0, { 1, assert(vim.api.nvim_get_current_line():find("InitialAlpha", 1, true)) - 1 })]])
  input("ci{")
  wait_for(function() return lua([[return vim.fn.mode():sub(1, 1) == "i"]]) end,
    "could not edit the preamble definition")
  lua([[vim.fn.complete(vim.api.nvim_win_get_cursor(0)[2] + 1, { "FinalAlpha", "FinalAlpine" })]])
  wait_for(function() return lua("return vim.fn.pumvisible() == 1") end,
    "preamble completion popup did not open")
  input("Fi")
  wait_for(function()
    return lua([[
      for _, event in ipairs(events) do
        if event.name == "TextChangedP" and event.popup == 1 then return vim.fn.pumvisible() == 1 end
      end
      return false
    ]])
  end, "preamble edit did not produce a real popup change")
  expect_frozen(250, "preamble completion typing")
  input("<C-n><C-y><Esc>")
  wait_for(function()
    return lua([[return vim.fn.mode() == "n" and vim.api.nvim_get_current_line() == ...]],
      [[\newcommand{\PreviewA}{FinalAlpha}]])
  end, "preamble completion selection did not finish")
  input("$x")
  wait_for(function()
    return lua([[return vim.api.nvim_get_current_line() == ...]], [[\newcommand{\PreviewA}{FinalAlpha]])
  end, "could not leave the preamble definition incomplete")
  lua([[vim.cmd("write")]])
  assert(vim.fn.readfile(preamble)[1] == [[\newcommand{\PreviewA}{FinalAlpha]],
    "incomplete preamble save did not reach disk")
  expect_frozen(500, "saving an incomplete preamble")
  input("A}<Esc>")
  wait_for(function()
    return lua([[return vim.fn.mode() == "n" and vim.api.nvim_get_current_line() == ...]],
      [[\newcommand{\PreviewA}{FinalAlpha}]])
  end, "could not restore the preamble definition in memory")

  lua([[
    local line = vim.api.nvim_buf_get_lines(0, 1, 2, false)[1]
    vim.api.nvim_win_set_cursor(0, { 2, assert(line:find("InitialBeta", 1, true)) - 1 })
  ]])
  input("ci{Stream")
  wait_for(function() return lua([[return vim.fn.mode():sub(1, 1) == "i"]]) end,
    "could not start continuous preamble typing")
  lua([[
    stream = { sent = 0, total = 60, finished = false }
    local timer = (vim.uv or vim.loop).new_timer()
    timer:start(10, 10, vim.schedule_wrap(function()
      stream.sent = stream.sent + 1
      vim.api.nvim_input("z")
      if stream.sent == stream.total then
        stream.finished = true
        timer:stop()
        timer:close()
      end
    end))
  ]])
  expect_frozen(900, "continuous preamble typing")
  local final_beta = "Stream" .. string.rep("z", 60)
  wait_for(function()
    return lua([[return stream.finished and vim.api.nvim_get_current_line() == ...]],
      [[\newcommand{\PreviewB}{]] .. final_beta .. "}")
  end, "continuous preamble typing did not reach its final text")
  input("<Esc>")
  wait_for(function() return lua("return vim.fn.mode() == 'n'") end, "could not leave preamble Insert mode")
  lua([[posts = {}; stream = nil; vim.api.nvim_set_current_buf(root_buf)]])
  wait_for(function()
    local raw = request(port)
    return raw:find("FinalAlpha", 1, true) and raw:find(final_beta, 1, true)
      and not raw:find("InitialAlpha", 1, true) and not raw:find("InitialBeta", 1, true)
  end, "returning to the document did not apply both final unsaved definitions")
  expect_frozen(250, "a late preamble callback after resume", request(port))
  local resumes = 0
  for _, post in ipairs(lua("return posts")) do
    if post.url:sub(-13) == "/editor-state" then
      local snapshot = vim.json.decode(post.body)
      if snapshot.file == root then
        resumes = resumes + 1
        local found = false
        for _, buffer in ipairs(snapshot.buffers or {}) do
          if buffer.file == preamble then
            found = buffer.source == [[\newcommand{\PreviewA}{FinalAlpha}]] .. "\n"
              .. [[\newcommand{\PreviewB}{]] .. final_beta .. "}"
          end
        end
        assert(found, "resume snapshot omitted or split the final preamble definitions")
      end
    end
  end
  assert(resumes == 1, "returning to the document did not send exactly one atomic resume snapshot")
  assert(lua("return vim.bo[preamble_buf].modified"), "resuming saved the preamble unexpectedly")
  assert(vim.fn.readfile(preamble)[1] == [[\newcommand{\PreviewA}{FinalAlpha]],
    "resuming overwrote the saved incomplete preamble")
  lua("vim.api.nvim_win_set_cursor(0, { 4, 0 })")
  input("ABodyStillLive")
  wait_for(function() return page(port):find("BodyStillLive", 1, true) ~= nil end,
    "body typing stopped updating after the preamble resumed")
  input("<Esc>")
  wait_for(function() return lua("return vim.fn.mode() == 'n'") end, "could not leave body Insert mode")
  lua("mp.stop()")
end, debug.traceback)

-- Stop only daemons spawned by this isolated editor, even if an assertion
-- failed midway through Insert mode or a completion menu.
pcall(lua, [[
  pcall(vim.api.nvim_exec_autocmds, "VimLeavePre", {})
  for _, job in ipairs(jobs or {}) do pcall(vim.fn.jobstop, job) end
  if jobs and #jobs > 0 then pcall(vim.fn.jobwait, jobs, 2000) end
]])
pcall(vim.fn.jobstop, child)
pcall(vim.fn.jobwait, { child }, 2000)
vim.fn.delete(scratch, "rf")
if not ok then error(err, 0) end
print("mathpreview completion-sync regression: ok")
