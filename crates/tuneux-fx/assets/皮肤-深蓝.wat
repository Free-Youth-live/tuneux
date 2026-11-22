(module
  (import "host" "theme_register" (func $theme_register (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "name=深蓝\nbg=#0000aa\nfg=#aaaaaa\nborder=#55ffff\nborder_type=double\nmenu_bg=#0000aa\nmenu_fg=#aaaaaa\nfkey_num=#ffff55\npassthrough=#ffffff\nresample=#999999\nbar_style=matrix\nbar_fg=#55ffff\npeak_fg=#ffffff\ngrid_fg=#999999\nlevel_low=#55ff55\nlevel_mid=#ffff00\nlevel_high=#ff0000\nband_low=#55ffff\nband_mid=#55ffff\nband_high=#ffff55\nblock_spectrum=false\nmonochrome=false\ndim_screen=false")
  (func (export "init") (param $slot i32) (result i32)
    (call $theme_register (i32.const 0) (i32.const 379)) drop
    (i32.const 0))
)
