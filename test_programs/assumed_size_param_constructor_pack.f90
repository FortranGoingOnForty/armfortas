! CHECK: ok
! IR_CHECK: alloca [i32 x 2]
! IR_NOT: alloca [i32 x 0]
! IR_CHECK: call @afs_array_pack
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|asm|obj|repro
program assumed_size_param_constructor_pack
  implicit none

  integer :: dim
  integer, parameter :: ndim = 2
  integer, parameter :: dims(*) = [(dim, dim = 1, ndim)]
  real, allocatable :: b(:,:), r(:)

  allocate(b(2,2), r(2))

  do dim = 1, ndim
    if (.not. all(shape(r) == pack(shape(b), dims /= dim))) error stop 1
  end do

  print *, "ok"
end program assumed_size_param_constructor_pack
