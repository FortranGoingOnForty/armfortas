! audit31 harvest: FINAL procedure declared in a module, used on a
! local derived-type variable inside a BLOCK in the program.
! Sprint 31 #496 made the BLOCK-scope finalization fire; this test
! checks that the finalizer is found through a cross-module use-
! association rather than only in-program scope, and runs at END
! BLOCK with the component value intact.
! CHECK: inside block
! CHECK: finalizer ran for id= 7
! CHECK: after block
module audit31_final_mod
  implicit none
  type :: counted_t
    integer :: id = 0
  contains
    final :: destroy_counted
  end type
contains
  subroutine destroy_counted(this)
    type(counted_t), intent(inout) :: this
    print *, 'finalizer ran for id=', this%id
  end subroutine
end module

program audit31_final_cross_module
  use audit31_final_mod
  implicit none
  block
    type(counted_t) :: c
    c%id = 7
    print *, 'inside block'
  end block
  print *, 'after block'
end program
