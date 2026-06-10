! l02: short-circuit is mandatory — exactly one arm evaluates, at
! every opt level (CSE/LICM must not hoist or merge the arms), and a
! conditional can guard a trapping access.
! FLAGS: --std=f2023
! CHECK: 1 1
! CHECK: 1002 2
! CHECK: -1
! OPT_EQ: O0,O1,O2,O3,Ofast => stdout|stderr|exit
program l02_conditional_short_circuit
  implicit none
  integer :: calls, x, i
  integer :: a(3)
  calls = 0
  a = [10, 20, 30]
  x = (a(1) > 5 ? bump(calls) : bump(calls) + 1000)
  print *, x, calls
  x = (a(1) > 50 ? bump(calls) : bump(calls) + 1000)
  print *, x, calls
  i = 7
  x = (i <= 3 ? a(i) : -1)
  print *, x
contains
  integer function bump(c)
    integer, intent(inout) :: c
    c = c + 1
    bump = c
  end function bump
end program l02_conditional_short_circuit
