;; logger -- proves a plugin can call back into the host with no capability
;; grant at all: `harness.log` is always permitted.
(module
  (import "harness" "log" (func $log (param i32 i32)))

  (memory (export "memory") 1)

  ;; The reply prefix. Data lives below the allocator's first page.
  (data (i32.const 16) "hello: ")

  (global $heap (mut i32) (i32.const 1024))

  (func $reserve (param $end i32)
    (local $have i32)
    (local.set $have (i32.mul (memory.size) (i32.const 65536)))
    (if (i32.gt_u (local.get $end) (local.get $have))
      (then
        (drop (memory.grow
          (i32.div_u
            (i32.add (i32.sub (local.get $end) (local.get $have)) (i32.const 65535))
            (i32.const 65536)))))))

  (func (export "alloc") (param $len i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $heap))
    (global.set $heap (i32.add (local.get $ptr) (i32.add (local.get $len) (i32.const 8))))
    (call $reserve (global.get $heap))
    (local.get $ptr))

  ;; Logs whatever the caller sent, then answers "hello: <request>".
  (func (export "run")
    (param $in i32) (param $in_len i32) (param $out i32) (param $out_cap i32)
    (result i32)
    (local $n i32)
    (call $log (local.get $in) (local.get $in_len))
    (local.set $n (i32.add (local.get $in_len) (i32.const 7)))
    (if (i32.gt_u (local.get $n) (local.get $out_cap))
      (then (return (i32.const -1))))
    (memory.copy (local.get $out) (i32.const 16) (i32.const 7))
    (memory.copy (i32.add (local.get $out) (i32.const 7)) (local.get $in) (local.get $in_len))
    (local.get $n)))
