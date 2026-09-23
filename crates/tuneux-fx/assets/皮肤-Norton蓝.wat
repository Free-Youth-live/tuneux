(module
  (import "host" "theme_register" (func $theme_register (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "name=Norton 蓝\nbg=#0000aa\nfg=#c0c0c0\nborder=#55ffff\nborder_type=double\nmenu_bg=#c0c0c0\nmenu_fg=#000000\nfkey_num=#ffff55\npassthrough=#ffffff\nresample=#5f5f5f\nbar_style=blocks\nbar_fg=#55ffff\npeak_fg=#ffff55\ngrid_fg=#005f5f\nlevel_low=#00aaaa\nlevel_mid=#ffff55\nlevel_high=#ff5555\nband_low=#00aaaa\nband_mid=#55ffff\nband_high=#ffff55")
  ;; init(slot)：注册「Norton 蓝」皮肤（插件只供色、宿主负责渲染）。
  ;; 皮肤文本 328 字节，含 19 个换行；slot 参数忽略（皮肤不占 DSP 槽位）。
  (func (export "init") (param $slot i32) (result i32)
    (call $theme_register (i32.const 0) (i32.const 328)) drop
    (i32.const 0))
)
