; ModuleID = 'probe5.7dc120f2c13478db-cgu.0'
source_filename = "probe5.7dc120f2c13478db-cgu.0"
target datalayout = "e-m:o-i64:64-i128:128-n32:64-S128"
target triple = "arm64-apple-macosx11.0.0"

@str.0 = internal unnamed_addr constant [28 x i8] c"attempt to add with overflow"
@alloc_2e38410fced2c310c68bdf2d45d0c3bd = private unnamed_addr constant <{ [4 x i8] }> <{ [4 x i8] c"\02\00\00\00" }>, align 4
@alloc_e6758488a51c40069ade2309416f0500 = private unnamed_addr constant <{ [6 x i8] }> <{ [6 x i8] c"<anon>" }>, align 1
@alloc_df0c956ba63b074469b752d22aa4c274 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_e6758488a51c40069ade2309416f0500, [16 x i8] c"\06\00\00\00\00\00\00\00\01\00\00\00+\00\00\00" }>, align 8

; <i32 as core::ops::arith::AddAssign<&i32>>::add_assign
; Function Attrs: inlinehint uwtable
define internal void @"_ZN66_$LT$i32$u20$as$u20$core..ops..arith..AddAssign$LT$$RF$i32$GT$$GT$10add_assign17h6b95ad71f19b463dE"(ptr align 4 %self, ptr align 4 %other, ptr align 8 %0) unnamed_addr #0 {
start:
  %other1 = load i32, ptr %other, align 4, !noundef !2
  %1 = load i32, ptr %self, align 4, !noundef !2
  %2 = call { i32, i1 } @llvm.sadd.with.overflow.i32(i32 %1, i32 %other1)
  %_4.0 = extractvalue { i32, i1 } %2, 0
  %_4.1 = extractvalue { i32, i1 } %2, 1
  %3 = call i1 @llvm.expect.i1(i1 %_4.1, i1 false)
  br i1 %3, label %panic, label %bb1

bb1:                                              ; preds = %start
  store i32 %_4.0, ptr %self, align 4
  ret void

panic:                                            ; preds = %start
; call core::panicking::panic
  call void @_ZN4core9panicking5panic17hc59c8a709a9b37aeE(ptr align 1 @str.0, i64 28, ptr align 8 %0) #5
  unreachable
}

; probe5::probe
; Function Attrs: uwtable
define void @_ZN6probe55probe17h8a24f7f3650db536E() unnamed_addr #1 {
start:
  %x = alloca i32, align 4
  store i32 1, ptr %x, align 4
; call <i32 as core::ops::arith::AddAssign<&i32>>::add_assign
  call void @"_ZN66_$LT$i32$u20$as$u20$core..ops..arith..AddAssign$LT$$RF$i32$GT$$GT$10add_assign17h6b95ad71f19b463dE"(ptr align 4 %x, ptr align 4 @alloc_2e38410fced2c310c68bdf2d45d0c3bd, ptr align 8 @alloc_df0c956ba63b074469b752d22aa4c274)
  ret void
}

; Function Attrs: nocallback nofree nosync nounwind speculatable willreturn memory(none)
declare { i32, i1 } @llvm.sadd.with.overflow.i32(i32, i32) #2

; Function Attrs: nocallback nofree nosync nounwind willreturn memory(none)
declare i1 @llvm.expect.i1(i1, i1) #3

; core::panicking::panic
; Function Attrs: cold noinline noreturn uwtable
declare void @_ZN4core9panicking5panic17hc59c8a709a9b37aeE(ptr align 1, i64, ptr align 8) unnamed_addr #4

attributes #0 = { inlinehint uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #1 = { uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #2 = { nocallback nofree nosync nounwind speculatable willreturn memory(none) }
attributes #3 = { nocallback nofree nosync nounwind willreturn memory(none) }
attributes #4 = { cold noinline noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #5 = { noreturn }

!llvm.module.flags = !{!0}
!llvm.ident = !{!1}

!0 = !{i32 8, !"PIC Level", i32 2}
!1 = !{!"rustc version 1.78.0 (9b00956e5 2024-04-29)"}
!2 = !{}
