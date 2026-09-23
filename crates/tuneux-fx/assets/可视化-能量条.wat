(module
  (import "host" "meter_read" (func $meter_read (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  ;; 静态模板区：行标签（各 4 字节）+ 全块条 / 全点条（各 30 字节）。
  (data (i32.const 4096) "低 ")
  (data (i32.const 4100) "中 ")
  (data (i32.const 4104) "高 ")
  (data (i32.const 4112) "██████████")
  (data (i32.const 4144) "░░░░░░░░░░")

  ;; 单桶能量→格数：读 [start, end) 段取最大值 ×10 四舍五入，钳 0..10。
  ;;（最大值而非平均：平均会把纯音 / 稀疏频谱的峰值稀释到零格。）
  (func $bucket_cells (param $start i32) (param $end i32) (result i32)
    (local $i i32) (local $max f32) (local $cells i32)
    ;; 空桶（n<3 时可能出现）防误读。
    (if (i32.eq (local.get $start) (local.get $end))
      (then (return (i32.const 0))))
    (local.set $i (local.get $start))
    (loop $lp
      (local.set $max (f32.max (local.get $max)
        (f32.load (i32.add (i32.const 2048) (i32.mul (local.get $i) (i32.const 4))))))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $lp (i32.lt_u (local.get $i) (local.get $end))))
    (local.set $cells
      (i32.trunc_f32_u
        (f32.add (f32.mul (local.get $max) (f32.const 10)) (f32.const 0.5))))
    (if (i32.gt_u (local.get $cells) (i32.const 10))
      (then (local.set $cells (i32.const 10))))
    (local.get $cells))

  ;; 写一行：标签 + cells 格块 + 剩余点 + 换行（行宽 35 字节）。
  (func $write_row (param $row i32) (param $cells i32)
    (local $base i32)
    (local.set $base (i32.add (i32.const 1024) (i32.mul (local.get $row) (i32.const 35))))
    (memory.copy (local.get $base)
      (i32.add (i32.const 4096) (i32.mul (local.get $row) (i32.const 4)))
      (i32.const 4))
    (memory.copy (i32.add (local.get $base) (i32.const 4))
      (i32.const 4112)
      (i32.mul (local.get $cells) (i32.const 3)))
    (memory.copy
      (i32.add (i32.add (local.get $base) (i32.const 4))
        (i32.mul (local.get $cells) (i32.const 3)))
      (i32.const 4144)
      (i32.mul (i32.sub (i32.const 10) (local.get $cells)) (i32.const 3)))
    (i32.store8 (i32.add (local.get $base) (i32.const 34)) (i32.const 10)))

  (func (export "init") (param $slot i32) (result i32) (i32.const 0))

  ;; tick：读 256 段频谱 → 低/中/高三桶平均 → 三行字符画写约定区。
  (func (export "tick") (result i32)
    (local $n i32) (local $b1 i32) (local $b2 i32)
    (local.set $n (call $meter_read (i32.const 2048) (i32.const 256)))
    (if (i32.eqz (local.get $n)) (then (return (i32.const 0))))
    (local.set $b1 (i32.div_u (local.get $n) (i32.const 3)))
    (local.set $b2 (i32.div_u (i32.mul (local.get $n) (i32.const 2)) (i32.const 3)))
    (call $write_row (i32.const 0) (call $bucket_cells (i32.const 0) (local.get $b1)))
    (call $write_row (i32.const 1) (call $bucket_cells (local.get $b1) (local.get $b2)))
    (call $write_row (i32.const 2) (call $bucket_cells (local.get $b2) (local.get $n)))
    ;; 约定区：[0,4) = 画面字节长度（3 行 × 35），[1024..) = 文本。
    (i32.store (i32.const 0) (i32.const 105))
    (i32.const 0))
)
