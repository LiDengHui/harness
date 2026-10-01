;; file_snoop -- asks the host for the workspace `Cargo.toml` and answers with
;; the bytes it got back.
;;
;; With `file_read` granted the host resolves "Cargo.toml" against its workspace
;; root and copies the file into the plugin's scratch buffer. Without the grant
;; the host refuses and this plugin's call fails -- the capability is checked in
;; the host, so a plugin cannot opt out of it by ignoring a return value.
(module
  (import "harness" "read_file"
    (func $read_file (param i32 i32 i32 i32) (result i64)))

  (memory (export "memory") 1)

  ;; The path this plugin asks for, below the allocator's first page.
  (data (i32.const 32) "Cargo.toml")

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

  (func (export "run")
    (param $in i32) (param $in_len i32) (param $out i32) (param $out_cap i32)
    (result i32)
    (local $scratch i32)
    (local $read i64)
    (local $len i32)
    ;; The scratch buffer is this plugin's own memory; the host only ever writes
    ;; as far as the capacity it was handed.
    (local.set $scratch (call $alloc (i32.const 8192)))
    (local.set $read
      (call $read_file
        (i32.const 32) (i32.const 10)
        (local.get $scratch) (i32.const 8192)))
    ;; Negative means "could not read it": the plugin reports that to its caller
    ;; rather than inventing an empty answer.
    (if (i64.lt_s (local.get $read) (i64.const 0))
      (then (return (i32.const -1))))
    (local.set $len (i32.wrap_i64 (local.get $read)))
    (if (i32.gt_u (local.get $len) (local.get $out_cap))
      (then (local.set $len (local.get $out_cap))))
    (memory.copy (local.get $out) (local.get $scratch) (local.get $len))
    (local.get $len)))
