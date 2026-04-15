! audit31 Finding 15: locally declared derived-type vars inside a
! BLOCK never ran their FINAL subroutines because the Stmt::Block
! lowering just restored the shadowed outer locals and returned.
! F2018 §7.5.6.3 / §9.7.3.2 require finalization and implicit
! deallocation at END BLOCK. Gather the block-introduced keys and
! route them through insert_implicit_dealloc before the restore
! step. Task #496.
! CHECK: finalizer id= 9
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
