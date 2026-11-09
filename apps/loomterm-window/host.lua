-- Internal Loomterm Window adapter. Never load a user's WezTerm configuration.
local wezterm = require 'wezterm'
wezterm.on('format-window-title', function() return 'Loomterm Window' end)
return {
  automatically_reload_config = false,
  check_for_updates = false,
  enable_tab_bar = false,
  disable_default_key_bindings = true,
  use_ime = true,
  -- Explicit diagnostics only; keep ordinary user input out of debug logs.
  debug_key_events = os.getenv('LOOMTERM_WINDOW_TRACE_KEYS') == '1',
  scrollback_lines = 0,
  exit_behavior = 'Close',
  window_close_confirmation = 'NeverPrompt',
  initial_cols = 120,
  initial_rows = 36,
  keys = {
    {key = 'C', mods = 'CTRL|SHIFT', action = wezterm.action.CopyTo 'Clipboard'},
    {key = 'V', mods = 'CTRL|SHIFT', action = wezterm.action.PasteFrom 'Clipboard'},
  },
}
