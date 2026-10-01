;; spin -- never returns. Used to prove that `SandboxLimits::timeout` interrupts
;; a compute-bound plugin instead of hanging the caller.
;;
;; The epoch checks the compiler inserts on loop back-edges are what make this
;; interruptible; an interpreter-level deadline could never fire here because
;; the wasm never yields to the host.
(module
  (memory (export "memory") 1)

  (global $heap (mut i32) (i32.const 1024))

  (func (export "alloc") (param $len i32) (result i32)
    (global.get $heap))

  (func (export "run")
    (param i32 i32 i32 i32) (result i32)
    (loop $forever (br $forever))
    unreachable))
