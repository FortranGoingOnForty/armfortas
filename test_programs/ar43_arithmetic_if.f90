! Arithmetic IF must evaluate its expression exactly once and branch to the
! negative, zero, or positive label without falling through.
!
! CHECK: -1 0 1 -1 0 1 -1 0 1 -1 0 1 12 3
! IR_CHECK: icmp lt
! IR_CHECK: fcmp lt
! IR_CHECK: cond_br
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module ar43_arithmetic_if_cleanup_m
  implicit none
  integer :: cleanup_count = 0

  type :: marker
  contains
    final :: finish_marker
  end type marker
contains
  subroutine finish_marker(value)
    type(marker), intent(inout) :: value
    cleanup_count = cleanup_count + 1
  end subroutine finish_marker

  subroutine check_arithmetic_if_cleanup
    cleanup_count = 0

    block
      type(marker) :: value
      if (-1) 100, 900, 900
      error stop 3
    end block
    error stop 4
100 continue
    if (cleanup_count /= 1) error stop 5

    block
      type(marker) :: value
      if (0) 900, 200, 900
      error stop 6
    end block
    error stop 7
200 continue
    if (cleanup_count /= 2) error stop 8

    block
      type(marker) :: value
      if (1) 900, 900, 300
      error stop 9
    end block
    error stop 10
300 continue
    if (cleanup_count /= 3) error stop 11
    return

900 error stop 12
  end subroutine check_arithmetic_if_cleanup
end module ar43_arithmetic_if_cleanup_m

program ar43_arithmetic_if
  use ar43_arithmetic_if_cleanup_m, only: check_arithmetic_if_cleanup, cleanup_count
  implicit none
  integer :: evals
  integer :: results(12)
  integer :: classify_i32, classify_i64, classify_r32, classify_r64

  evals = 0
  results(1) = classify_i32(-7, evals)
  results(2) = classify_i32(0, evals)
  results(3) = classify_i32(9, evals)
  results(4) = classify_i64(-8589934592_8, evals)
  results(5) = classify_i64(0_8, evals)
  results(6) = classify_i64(8589934592_8, evals)
  results(7) = classify_r32(-1.5_4, evals)
  results(8) = classify_r32(-0.0_4, evals)
  results(9) = classify_r32(2.5_4, evals)
  results(10) = classify_r64(-1.0e300_8, evals)
  results(11) = classify_r64(-0.0_8, evals)
  results(12) = classify_r64(1.0e300_8, evals)
  call check_arithmetic_if_cleanup()

  print *, results, evals, cleanup_count
  if (any(results /= [-1, 0, 1, -1, 0, 1, -1, 0, 1, -1, 0, 1])) &
    error stop 1
  if (evals /= 12) error stop 2
end program ar43_arithmetic_if

integer function probe_i32(value, evals)
  implicit none
  integer, intent(in) :: value
  integer, intent(inout) :: evals
  evals = evals + 1
  probe_i32 = value
end function probe_i32

integer function classify_i32(value, evals)
  implicit none
  integer, intent(in) :: value
  integer, intent(inout) :: evals
  integer :: probe_i32
  if (probe_i32(value, evals)) 10, 20, 30
10 classify_i32 = -1
  return
20 classify_i32 = 0
  return
30 classify_i32 = 1
end function classify_i32

integer(kind=8) function probe_i64(value, evals)
  implicit none
  integer(kind=8), intent(in) :: value
  integer, intent(inout) :: evals
  evals = evals + 1
  probe_i64 = value
end function probe_i64

integer function classify_i64(value, evals)
  implicit none
  integer(kind=8), intent(in) :: value
  integer, intent(inout) :: evals
  integer(kind=8) :: probe_i64
  if (probe_i64(value, evals)) 10, 20, 30
10 classify_i64 = -1
  return
20 classify_i64 = 0
  return
30 classify_i64 = 1
end function classify_i64

real function probe_r32(value, evals)
  implicit none
  real, intent(in) :: value
  integer, intent(inout) :: evals
  evals = evals + 1
  probe_r32 = value
end function probe_r32

integer function classify_r32(value, evals)
  implicit none
  real, intent(in) :: value
  integer, intent(inout) :: evals
  real :: probe_r32
  if (probe_r32(value, evals)) 10, 20, 30
10 classify_r32 = -1
  return
20 classify_r32 = 0
  return
30 classify_r32 = 1
end function classify_r32

real(kind=8) function probe_r64(value, evals)
  implicit none
  real(kind=8), intent(in) :: value
  integer, intent(inout) :: evals
  evals = evals + 1
  probe_r64 = value
end function probe_r64

integer function classify_r64(value, evals)
  implicit none
  real(kind=8), intent(in) :: value
  integer, intent(inout) :: evals
  real(kind=8) :: probe_r64
  if (probe_r64(value, evals)) 10, 20, 30
10 classify_r64 = -1
  return
20 classify_r64 = 0
  return
30 classify_r64 = 1
end function classify_r64
