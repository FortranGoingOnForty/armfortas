! l02a item 4: F2023 conditional actual arguments in FUNCTION references.
! The fn-call argument path selects the association per arm
! (lower_call_arg_maybe_conditional), the same as a CALL — never a value
! temp — so INTENT(OUT)/INOUT writes land in the chosen actual.
! FLAGS: --std=f2023
program l02a_condarg_fn
  implicit none
  integer :: a, r, p1, p2
  logical :: c
  a = 3

  ! intent(in) conditional arg
  r = twice((a > 0 ? a : 1))
  print '(I0)', r
  ! CHECK: 6

  ! intent(out) through a conditional arg: the write must reach the
  ! selected actual, not a value temporary.
  p1 = -1; p2 = -1
  c = .true.
  r = setto((c ? p1 : p2), 42)
  print '(2(I0,1X))', p1, p2
  ! CHECK: 42 -1
  p1 = -1; p2 = -1
  c = .false.
  r = setto((c ? p1 : p2), 99)
  print '(2(I0,1X))', p1, p2
  ! CHECK: -1 99
  ! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|exit
contains
  integer function twice(x)
    integer, intent(in) :: x
    twice = 2 * x
  end function twice
  integer function setto(y, v)
    integer, intent(out) :: y
    integer, intent(in) :: v
    y = v
    setto = 0
  end function setto
end program l02a_condarg_fn
