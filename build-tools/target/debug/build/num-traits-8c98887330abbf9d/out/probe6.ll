; ModuleID = 'probe6.545c20a9f65f5172-cgu.0'
source_filename = "probe6.545c20a9f65f5172-cgu.0"
target datalayout = "e-m:o-i64:64-i128:128-n32:64-S128"
target triple = "arm64-apple-macosx11.0.0"

@str.0 = internal unnamed_addr constant [25 x i8] c"attempt to divide by zero"
@alloc_e6758488a51c40069ade2309416f0500 = private unnamed_addr constant <{ [6 x i8] }> <{ [6 x i8] c"<anon>" }>, align 1
@alloc_5491181a412bbabfebff41ab96c406e4 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_e6758488a51c40069ade2309416f0500, [16 x i8] c"\06\00\00\00\00\00\00\00\01\00\00\00\1F\00\00\00" }>, align 8

; probe6::probe
; Function Attrs: uwtable
define void @_ZN6probe65probe17h91af924479a20797E() unnamed_addr #0 {
start:
  %0 = call i1 @llvm.expect.i1(i1 false, i1 false)
  br i1 %0, label %panic.i, label %"_ZN4core3num21_$LT$impl$u20$u32$GT$10div_euclid17h7cd4c762d3389636E.exit"

panic.i:                                          ; preds = %start
; call core::panicking::panic
  call void @_ZN4core9panicking5panic17hc59c8a709a9b37aeE(ptr align 1 @str.0, i64 25, ptr align 8 @alloc_5491181a412bbabfebff41ab96c406e4) #3
  unreachable

"_ZN4core3num21_$LT$impl$u20$u32$GT$10div_euclid17h7cd4c762d3389636E.exit": ; preds = %start
  ret void
}

; Function Attrs: nocallback nofree nosync nounwind willreturn memory(none)
declare i1 @llvm.expect.i1(i1, i1) #1

; core::panicking::panic
; Function Attrs: cold noinline noreturn uwtable
declare void @_ZN4core9panicking5panic17hc59c8a709a9b37aeE(ptr align 1, i64, ptr align 8) unnamed_addr #2

attributes #0 = { uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #1 = { nocallback nofree nosync nounwind willreturn memory(none) }
attributes #2 = { cold noinline noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #3 = { noreturn }

!llvm.module.flags = !{!0}
!llvm.ident = !{!1}

!0 = !{i32 8, !"PIC Level", i32 2}
!1 = !{!"rustc version 1.78.0 (9b00956e5 2024-04-29)"}
