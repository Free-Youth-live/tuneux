(module
  (import "host" "eq_set" (func $eq_set (param i32 i32 f32) (result i32)))
  (import "host" "log" (func $log (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "tuneux-eq")
  ;; init(slot)：写 10 段 0 dB（直通）到被分配的槽位，打一条加载日志。
  (func (export "init") (param $slot i32) (result i32)
    (call $eq_set (local.get $slot) (i32.const 0) (f32.const 0.0)) drop
    (call $eq_set (local.get $slot) (i32.const 1) (f32.const 0.0)) drop
    (call $eq_set (local.get $slot) (i32.const 2) (f32.const 0.0)) drop
    (call $eq_set (local.get $slot) (i32.const 3) (f32.const 0.0)) drop
    (call $eq_set (local.get $slot) (i32.const 4) (f32.const 0.0)) drop
    (call $eq_set (local.get $slot) (i32.const 5) (f32.const 0.0)) drop
    (call $eq_set (local.get $slot) (i32.const 6) (f32.const 0.0)) drop
    (call $eq_set (local.get $slot) (i32.const 7) (f32.const 0.0)) drop
    (call $eq_set (local.get $slot) (i32.const 8) (f32.const 0.0)) drop
    (call $eq_set (local.get $slot) (i32.const 9) (f32.const 0.0)) drop
    (call $log (i32.const 0) (i32.const 9)) drop
    (i32.const 0))
  ;; set_band(slot, band, gain_db)：控制面遥控器——转发给宿主写槽位。
  (func (export "set_band") (param i32 i32 f32) (result i32)
    (call $eq_set (local.get 0) (local.get 1) (local.get 2)))
)
