;; A guest plugin in WebAssembly text (see the guest ABI in the cordis-hmr
;; docs). Edit the greeting while the app runs: hmr reloads this module in
;; place, disposing the old instance first.
(module
  (import "cordis" "write" (func $write (param i32 i32)))
  (import "cordis" "log" (func $log (param i32 i32 i32)))
  (memory (export "memory") 1)

  (data (i32.const 0) "{\"plugin\":{\"name\":\"hello\"}}\00")
  (data (i32.const 128) "goodbye from wasm\00")
  (data (i32.const 256) "hello from wasm\00")

  ;; the length of the NUL-terminated string at $ptr
  (func $len (param $ptr i32) (result i32)
    (local $end i32)
    (local.set $end (local.get $ptr))
    (block $done
      (loop $next
        (br_if $done (i32.eqz (i32.load8_u (local.get $end))))
        (local.set $end (i32.add (local.get $end) (i32.const 1)))
        (br $next)))
    (i32.sub (local.get $end) (local.get $ptr)))

  (func $info (param $ptr i32)
    (call $log (i32.const 2) (local.get $ptr) (call $len (local.get $ptr))))

  (func (export "cordis_meta") (result i32)
    (call $write (i32.const 0) (call $len (i32.const 0)))
    (i32.const 0))

  (func (export "cordis_apply") (param i32) (result i32)
    (call $info (i32.const 256))
    (i32.const 0))

  (func (export "cordis_dispose")
    (call $info (i32.const 128))))
