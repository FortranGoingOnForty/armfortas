! l07: a type-bound procedure whose target is a separate module procedure
! (the SMP body lives in a submodule). Exercises the TBP-thunk ownership
! rule — the thunk must have exactly one owning TU across the parent
! module and the submodule. Single-file form for the full opt matrix.
! FLAGS: --std=f2023
module l07tbp
  implicit none
  type :: counter
    integer :: n = 0
  contains
    procedure :: bump
  end type
  interface
    module subroutine bump(self, by)
      class(counter), intent(inout) :: self
      integer, intent(in) :: by
    end subroutine
  end interface
end module
submodule (l07tbp) l07tbp_impl
contains
  module procedure bump
    self%n = self%n + by
  end procedure
end submodule
program l07_tbp_smp_target
  use l07tbp
  implicit none
  type(counter) :: c
  call c%bump(5)
  call c%bump(7)
  print '(I0)', c%n
  ! CHECK: 12
  ! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|exit
end program l07_tbp_smp_target
