-- plugin/mathpreview.lua — auto-sourced by nvim on startup. Registers the
-- :MathPreview command family so users don't have to call setup() before
-- they can start the daemon. The real implementation is lazy-required
-- from `lua/mathpreview/init.lua` on first invocation so plugin start-up
-- stays cheap.

if vim.g.loaded_mathpreview == 1 then return end
vim.g.loaded_mathpreview = 1

vim.api.nvim_create_user_command("MathPreview", function(cmd)
  -- Optional argument picks the viewer for this (and later) previews:
  --   :MathPreview           use the configured default (setup{viewer=…})
  --   :MathPreview window    open in the native Locus window
  --   :MathPreview browser   open in a browser tab
  require("mathpreview").start({ viewer = cmd.args ~= "" and cmd.args or nil })
end, {
  nargs = "?",
  complete = function(arglead)
    return vim.tbl_filter(function(v) return v:find(arglead, 1, true) == 1 end,
      { "browser", "window" })
  end,
  desc = "Start the mathpreview daemon (optionally: MathPreview window|browser)",
})

vim.api.nvim_create_user_command("MathPreviewStop", function()
  require("mathpreview").stop()
end, { desc = "Stop the mathpreview daemon" })

vim.api.nvim_create_user_command("MathPreviewRestart", function()
  require("mathpreview").restart()
end, { desc = "Restart the mathpreview daemon" })

vim.api.nvim_create_user_command("MathPreviewClean", function()
  require("mathpreview").clean()
end, { desc = "Find and stop abandoned mathpreview daemons (no editor, no viewer)" })

vim.api.nvim_create_user_command("MathPreviewStatus", function()
  print(vim.inspect(require("mathpreview").status()))
end, { desc = "Show mathpreview daemon and plugin status" })

vim.api.nvim_create_user_command("MathPreviewDebug", function()
  require("mathpreview").debug()
end, { desc = "Show mathpreview resolved settings and config/macro paths" })
