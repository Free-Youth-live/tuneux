(module
  (import "host" "compressor_set" (func $compressor_set (param i32 i32 f32) (result i32)))
  (import "host" "log" (func $log (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "tuneux-comp")
  ;; init(slot)：写默认压缩器参数到被分配的槽位。
  (func (export "init") (param $slot i32) (result i32)
    (call $compressor_set (local.get $slot) (i32.const 0) (f32.const -20.0)) drop
    (call $compressor_set (local.get $slot) (i32.const 1) (f32.const 4.0)) drop
    (call $compressor_set (local.get $slot) (i32.const 2) (f32.const 10.0)) drop
    (call $compressor_set (local.get $slot) (i32.const 3) (f32.const 100.0)) drop
    (call $compressor_set (local.get $slot) (i32.const 4) (f32.const 0.0)) drop
    (call $log (i32.const 0) (i32.const 11)) drop
    (i32.const 0))
  ;; set(slot, param, value)：控制面遥控器。
  (func (export "set") (param i32 i32 f32) (result i32)
    (call $compressor_set (local.get 0) (local.get 1) (local.get 2)))
)
