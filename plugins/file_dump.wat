;; file_dump -- writes its input into the workspace as `dump.txt`.
;;
;; The path is fixed, but the content is whatever the caller sent, so the host's
;; containment guard still applies to the path and the plugin has no way to pick
;; a different one.
(module
  (import "harness" "write_file"
    (func $write_file (param i32 i32 i32 i32) (result i64)))

  (memory (export "memory") 1)

  (data (i32.const 32) "dump.txt")

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

  (func $alloc (export "alloc") (param $len i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $heap))
    (global.set $heap (i32.add (local.get $ptr) (i32.add (local.get $len) (i32.const 8))))
    (call $reserve (global.get $heap))
    (local.get $ptr))

  ;; Writes the request to `dump.txt` and answers with the request once the host
  ;; confirms how much actually landed on disk.
  (func (export "run")
    (param $in i32) (param $in_len i32) (param $out i32) (param $out_cap i32)
    (result i32)
    (local $written i64)
    (local.set $written
      (call $write_file
        (i32.const 32) (i32.const 8)
        (local.get $in) (local.get $in_len)))
    (if (i64.lt_s (local.get $written) (i64.const 0))
      (then (return (i32.const -1))))
    (local.set $in_len (i32.wrap_i64 (local.get $written)))
    (if (i32.gt_u (local.get $in_len) (local.get $out_cap))
      (then (local.set $in_len (local.get $out_cap))))
    (memory.copy (local.get $out) (local.get $in) (local.get $in_len))
    (local.get $in_len)))
