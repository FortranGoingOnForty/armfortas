! A PURE wrapper inherits mutable non-argument state read by an internal callee.
! Both functions are recursive so the inliner cannot erase the call-graph edge
! before GVN classifies the wrapper.
module ar42_gvn_transitive_global_mod
  implicit none
  integer :: state = 0
contains
  pure recursive integer function read_state(depth) result(value)
    integer, intent(in) :: depth
    if (depth <= 0) then
      value = state
    else
      value = read_state(depth - 1)
    end if
  end function read_state

  pure recursive integer function wrapper(depth) result(value)
    integer, intent(in) :: depth
    if (depth <= 0) then
      value = read_state(0)
    else
      value = wrapper(depth - 1)
    end if
  end function wrapper
end module ar42_gvn_transitive_global_mod

program ar42_gvn_transitive_global
  use ar42_gvn_transitive_global_mod
  implicit none
  integer :: before, after

  before = wrapper(1)
  state = 100
  after = wrapper(1)
  print *, before, after
end program ar42_gvn_transitive_global
! CHECK: 0 100
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
