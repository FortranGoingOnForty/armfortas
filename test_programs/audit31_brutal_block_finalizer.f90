! XFAIL: BLOCK-scope finalizer does not fire — Task #496
! CHECK: finalizer id= 9
! audit31: BLOCK-scope finalizer doesn't fire at END BLOCK.
! Fortran 2018 §7.5.6 specifies finalization at END BLOCK.
! Expected output includes "finalizer id= 9" between "inside" and "after".
program audit31_block_finalizer
  implicit none
  type :: counted_t
    integer :: id = 0
  contains
    final :: destroy_counted
  end type

  print *, 'before'
  block
    type(counted_t) :: c
    c%id = 9
    print *, 'inside'
  end block
  print *, 'after'
contains
  subroutine destroy_counted(this)
    type(counted_t), intent(inout) :: this
    print *, 'finalizer id=', this%id
  end subroutine
end program
