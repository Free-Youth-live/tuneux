(module
  (import "host" "theme_register" (func $theme_register (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "name=午夜蓝\nbg=#0d1321\nfg=#c8c8c8\nborder=#5f87af\nborder_type=double\nmenu_bg=#3a4a5f\nmenu_fg=#e8e8e8\nfkey_num=#ffb000\npassthrough=#ffffff\nresample=#5f5f5f\nbar_style=hanzi\nbar_fg=#87afaf\npeak_fg=#ffb000\ngrid_fg=#3a4a5f\nlevel_low=#87af5f\nlevel_mid=#ffaf00\nlevel_high=#ff6c6b\nband_low=#5f87af\nband_mid=#87afaf\nband_high=#ffb000")
  ;; init(slot)：注册「午夜蓝」皮肤（插件只供色、宿主负责渲染）。
  ;; 皮肤文本 326 字节，含 19 个换行；slot 参数忽略（皮肤不占 DSP 槽位）。
  (func (export "init") (param $slot i32) (result i32)
    (call $theme_register (i32.const 0) (i32.const 326)) drop
    (i32.const 0))
)
