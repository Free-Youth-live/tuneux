(module
  (import "host" "theme_register" (func $theme_register (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "name=黑白\nbg=#ffffff\nfg=#000000\nborder=#000000\nborder_type=plain\nmenu_bg=#ffffff\nmenu_fg=#000000\nfkey_num=#000000\npassthrough=#000000\nresample=#808080\nbar_style=blocks\nbar_fg=#000000\npeak_fg=#808080\ngrid_fg=#808080\nlevel_low=#000000\nlevel_mid=#000000\nlevel_high=#000000\nband_low=#000000\nband_mid=#000000\nband_high=#000000\nblock_spectrum=true\nmonochrome=true\ndim_screen=false")
  (func (export "init") (param $slot i32) (result i32)
    (call $theme_register (i32.const 0) (i32.const 376)) drop
    (i32.const 0))
)
