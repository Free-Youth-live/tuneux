(module
  (import "host" "theme_register" (func $theme_register (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "name=灰白\nbg=#c8cdc5\nfg=#1b1b19\nborder=#373735\nborder_type=plain\nmenu_bg=#c8cdc5\nmenu_fg=#1b1b19\nfkey_num=#1b1b19\npassthrough=#5c5c58\nresample=#82827e\nbar_style=matrix\nbar_fg=#1b1b19\npeak_fg=#82827e\ngrid_fg=#82827e\nlevel_low=#1b1b19\nlevel_mid=#1b1b19\nlevel_high=#1b1b19\nband_low=#373735\nband_mid=#1b1b19\nband_high=#1b1b19\nblock_spectrum=false\nmonochrome=true\ndim_screen=false")
  (func (export "init") (param $slot i32) (result i32)
    (call $theme_register (i32.const 0) (i32.const 377)) drop
    (i32.const 0))
)
