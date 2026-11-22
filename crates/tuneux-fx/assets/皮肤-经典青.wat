(module
  (import "host" "theme_register" (func $theme_register (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "name=经典青\nbg=none\nfg=none\nborder=cyan\nborder_type=double\nmenu_bg=none\nmenu_fg=none\nfkey_num=#5f5f5f\npassthrough=#ffffff\nresample=#5f5f5f\nbar_style=matrix\nbar_fg=#55ff55\npeak_fg=#ffffff\ngrid_fg=#5f5f5f\nlevel_low=#00aa00\nlevel_mid=#ffff00\nlevel_high=#ff0000\nband_low=#0000ff\nband_mid=#55ff55\nband_high=#55ff55\nblock_spectrum=false\nmonochrome=false\ndim_screen=false")
  (func (export "init") (param $slot i32) (result i32)
    (call $theme_register (i32.const 0) (i32.const 367)) drop
    (i32.const 0))
)
