;; memory_hog -- grows its linear memory until the host refuses, then answers
;; with the final page count so the caller can see where the ceiling was.
;;
;; `memory.grow` returns -1 once the store's `StoreLimits` says no, so this loop
;; ends instead of running forever; a plugin without a limiter would keep
;; allocating until the process died.
(module
  (memory (export "memory") 1)

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

  (func (export "run")
    (param $in i32) (param $in_len i32) (param $out i32) (param $out_cap i32)
    (result i32)
    (loop $grow
      (br_if $grow (i32.ne (memory.grow (i32.const 1)) (i32.const -1))))
    (i32.store (local.get $out) (memory.size))
    (i32.const 4)))
