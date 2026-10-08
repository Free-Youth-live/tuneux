(module
  (import "host" "meter_read" (func $meter_read (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "init") (param $slot i32) (result i32) (i32.const 0))
  (func (export "tick") (result i32)
    (local $i i32) (local $v f32) (local $level i32)
    (local $out i32) (local $len i32) (local $row i32)
    ;; 读 256 段频谱
    (drop (call $meter_read (i32.const 256) (i32.const 256)))
    ;; ═══ 历史上移：row[0..6] ← row[1..7]（字节拷贝 128B/行） ═══
    (local.set $row (i32.const 0))
    (loop $shift_row
      (if (i32.lt_u (local.get $row) (i32.const 7))
        (then
          (local.set $i (i32.const 0))
          (loop $shift_byte
            (if (i32.lt_u (local.get $i) (i32.const 128))
              (then
                (i32.store8
                  (i32.add (i32.add (i32.const 4096)
                    (i32.mul (local.get $row) (i32.const 128))) (local.get $i))
                  (i32.load8_u
                    (i32.add (i32.add (i32.const 4096)
                      (i32.mul (i32.add (local.get $row) (i32.const 1)) (i32.const 128)))
                      (local.get $i))))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $shift_byte))))
          (local.set $row (i32.add (local.get $row) (i32.const 1)))
          (br $shift_row))))
    ;; ═══ 当前帧量化到行 7（前 32 段 × 8 级） ═══
    (local.set $i (i32.const 0))
    (loop $quantize
      (if (i32.lt_u (local.get $i) (i32.const 32))
        (then
          (local.set $v (f32.load
            (i32.add (i32.const 256) (i32.mul (local.get $i) (i32.const 4)))))
          (local.set $level
            (i32.trunc_f32_s (f32.mul (local.get $v) (f32.const 8.0))))
          (if (i32.lt_s (local.get $level) (i32.const 0))
            (then (local.set $level (i32.const 0))))
          (if (i32.gt_s (local.get $level) (i32.const 7))
            (then (local.set $level (i32.const 7))))
          (i32.store
            (i32.add (i32.const 4992) (i32.mul (local.get $i) (i32.const 4)))
            (local.get $level))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $quantize))))
    ;; ═══ 渲染 8 行 × 32 列块字符 ═══
    (local.set $out (i32.const 1024))
    (local.set $len (i32.const 0))
    (local.set $row (i32.const 0))
    (loop $render_row
      (if (i32.lt_u (local.get $row) (i32.const 8))
        (then
          (local.set $i (i32.const 0))
          (loop $render_col
            (if (i32.lt_u (local.get $i) (i32.const 32))
              (then
                (local.set $level (i32.load
                  (i32.add (i32.add (i32.const 4096)
                    (i32.mul (local.get $row) (i32.const 128)))
                    (i32.mul (local.get $i) (i32.const 4)))))
                (i32.store8 (local.get $out) (i32.const 226))
                (i32.store8 (i32.add (local.get $out) (i32.const 1)) (i32.const 150))
                (i32.store8 (i32.add (local.get $out) (i32.const 2))
                  (i32.add (i32.const 129) (local.get $level)))
                (local.set $out (i32.add (local.get $out) (i32.const 3)))
                (local.set $len (i32.add (local.get $len) (i32.const 3)))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $render_col))))
          (if (i32.lt_u (local.get $row) (i32.const 7))
            (then
              (i32.store8 (local.get $out) (i32.const 10))
              (local.set $out (i32.add (local.get $out) (i32.const 1)))
              (local.set $len (i32.add (local.get $len) (i32.const 1)))))
          (local.set $row (i32.add (local.get $row) (i32.const 1)))
          (br $render_row))))
    (i32.store (i32.const 0) (local.get $len))
    (i32.const 0)))
