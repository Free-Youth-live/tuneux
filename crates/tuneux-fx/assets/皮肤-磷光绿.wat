(module
  (import "host" "theme_register" (func $theme_register (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "name=磷光绿\nbg=#000000\nfg=#55ff55\nborder=#55ff55\nborder_type=double\nmenu_bg=#000000\nmenu_fg=#55ff55\nfkey_num=#ffffff\npassthrough=#ffffff\nresample=#00aa00\nbar_style=matrix\nbar_fg=#55ff55\npeak_fg=#ffffff\ngrid_fg=#00aa00\nlevel_low=#55ff55\nlevel_mid=#ffff00\nlevel_high=#ff0000\nband_low=#00aa00\nband_mid=#55ff55\nband_high=#55ff55\nblock_spectrum=false\nmonochrome=false\ndim_screen=false")
  (func (export "init") (param $slot i32) (result i32)
    (call $theme_register (i32.const 0) (i32.const 382)) drop
    (i32.const 0))
)
