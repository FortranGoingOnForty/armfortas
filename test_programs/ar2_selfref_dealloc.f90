! CHECK: selfref ok 3
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! MULTIFILE_LINK: ar2_selfref_mod.f90 ar2_selfref_main.f90

!--- file: ar2_selfref_mod.f90
module ar2_selfref_mod
  implicit none

  type :: node
    integer :: id = 0
    type(node), allocatable :: children(:)
  end type node
contains
  recursive subroutine grow(root, levels)
    type(node), intent(inout) :: root
    integer, intent(in) :: levels

    root%id = levels
    if (levels <= 1) then
      return
    end if

    allocate(root%children(1))
    call grow(root%children(1), levels - 1)
  end subroutine grow

  recursive function depth(root) result(n)
    type(node), intent(in) :: root
    integer :: n

    if (allocated(root%children)) then
      n = 1 + depth(root%children(1))
    else
      n = 1
    end if
  end function depth
end module ar2_selfref_mod

!--- file: ar2_selfref_main.f90
program ar2_selfref_dealloc
  use ar2_selfref_mod, only: node, grow, depth
  implicit none

  type(node), allocatable :: root

  allocate(root)
  call grow(root, 3)
  print '(a,1x,i0)', 'selfref ok', depth(root)
  deallocate(root)
end program ar2_selfref_dealloc
