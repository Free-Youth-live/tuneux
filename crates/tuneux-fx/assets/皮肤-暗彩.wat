(module
  (import "host" "theme_register" (func $theme_register (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "name=暗彩\nbg=#9ca494\nfg=#2d352d\nborder=#4d574e\nborder_type=plain\nmenu_bg=#9ca494\nmenu_fg=#2d352d\nfkey_num=#41483e\npassthrough=#495745\nresample=#666e5f\nbar_style=matrix\nbar_fg=#41706c\npeak_fg=#755b70\ngrid_fg=#666e5f\nlevel_low=#48734e\nlevel_mid=#9a8a3a\nlevel_high=#a04038\nband_low=#4d574e\nband_mid=#41706c\nband_high=#856537\nblock_spectrum=false\nmonochrome=false\ndim_screen=true")
  (func (export "init") (param $slot i32) (result i32)
    (call $theme_register (i32.const 0) (i32.const 377)) drop
    (i32.const 0))
)
