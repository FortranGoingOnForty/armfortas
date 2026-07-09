! MULTIFILE_LINK: ar12_only_defassign_t.f90 ar12_only_defassign_e.f90 ar12_only_defassign_main.f90
! CHECK: alloc=T

!--- file: ar12_only_defassign_t.f90
module ar12_only_defassign_t
  implicit none
  type :: v
    integer, allocatable :: a(:)
  end type
  interface assignment(=)
    module procedure asn
  end interface
contains
  function mk() result(r)
    type(v) :: r
    allocate(r%a(1))
    r%a(1) = 9
  end function

  subroutine asn(lhs, rhs)
    type(v), intent(out) :: lhs
    type(v), intent(in) :: rhs
    if (allocated(rhs%a)) then
      print '(a)', 'defined assignment fired'
    end if
  end subroutine
end module

!--- file: ar12_only_defassign_e.f90
module ar12_only_defassign_e
  use ar12_only_defassign_t, only: v, mk
  implicit none
contains
  function go() result(r)
    type(v) :: r
    r = mk()
  end function
end module

!--- file: ar12_only_defassign_main.f90
program ar12_only_defassign
  use ar12_only_defassign_t, only: v
  use ar12_only_defassign_e, only: go
  implicit none
  type(v) :: x
  x = go()
  print '(a,l1)', 'alloc=', allocated(x%a)
  if (.not. allocated(x%a)) stop 1
end program
