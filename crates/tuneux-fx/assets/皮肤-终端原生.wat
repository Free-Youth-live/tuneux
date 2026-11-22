(module
  (import "host" "theme_register" (func $theme_register (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "name=终端原生\nbg=none\nfg=none\nborder=none\nborder_type=plain\nmenu_bg=none\nmenu_fg=none\nfkey_num=none\npassthrough=#ffffff\nresample=none\nbar_style=matrix\nbar_fg=none\npeak_fg=none\ngrid_fg=none\nlevel_low=none\nlevel_mid=#ffff00\nlevel_high=#ff0000\nband_low=none\nband_mid=none\nband_high=none\nblock_spectrum=false\nmonochrome=false\ndim_screen=false")
  (func (export "init") (param $slot i32) (result i32)
    (call $theme_register (i32.const 0) (i32.const 342)) drop
    (i32.const 0))
)
