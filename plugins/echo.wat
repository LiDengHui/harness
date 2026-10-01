;; echo -- the smallest possible plugin, used to pin down the calling convention.
;;
;; Convention (documented in full at crates/harness-sandbox/src/lib.rs):
;;   * the module exports `memory`, `alloc(len) -> ptr`, and the entry point
;;     named in its manifest (here `run`);
;;   * the host calls `alloc` for the request, writes it there, calls `alloc`
;;     again for the reply buffer, then calls
;;     `run(in_ptr, in_len, out_ptr, out_cap) -> bytes written to out_ptr`;
;;   * a negative return value means the plugin gave up.
(module
  (memory (export "memory") 1)

  ;; Bump pointer. The first page below 1024 is left for the plugin's own
  ;; scratch data; everything above it belongs to `alloc`.
  (global $heap (mut i32) (i32.const 1024))

  ;; Grows memory until `end` is addressable. A failed grow is deliberately
  ;; ignored: the store's limiter is what decides how big a plugin may get, and
  ;; the out-of-bounds access that follows traps the call, which is the outcome
  ;; the host wants anyway.
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
    (local $n i32)
    (local.set $n (local.get $in_len))
    (if (i32.gt_u (local.get $n) (local.get $out_cap))
      (then (local.set $n (local.get $out_cap))))
    (memory.copy (local.get $out) (local.get $in) (local.get $n))
    (local.get $n)))
