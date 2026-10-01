;; http_fetch -- takes a URL on its input and answers with the response body.
;;
;; The URL is guest-controlled on purpose: the host is the one that checks
;; `network_access` and the url scheme, so a plugin cannot reach the network by
;; refusing to co-operate.
(module
  (import "harness" "http_get"
    (func $http_get (param i32 i32 i32 i32) (result i64)))

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
    (local $fetched i64)
    (local $len i32)
    (local.set $scratch (call $alloc (i32.const 8192)))
    (local.set $fetched
      (call $http_get
        (local.get $in) (local.get $in_len)
        (local.get $scratch) (i32.const 8192)))
    (if (i64.lt_s (local.get $fetched) (i64.const 0))
      (then (return (i32.const -1))))
    (local.set $len (i32.wrap_i64 (local.get $fetched)))
    (if (i32.gt_u (local.get $len) (local.get $out_cap))
      (then (local.set $len (local.get $out_cap))))
    (memory.copy (local.get $out) (local.get $scratch) (local.get $len))
    (local.get $len)))
