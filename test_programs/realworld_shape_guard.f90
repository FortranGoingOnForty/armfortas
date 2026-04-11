! runtime-shape guard over allocatable work array metadata.
! CHECK: 6 0 5 12 36
! IR_CHECK: call @afs_array_size(
! IR_CHECK: call @afs_array_lbound(
! IR_CHECK: call @afs_array_ubound(
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_shape_guard
  implicit none
  integer, allocatable :: work(:)
  integer :: n, lo, hi, i, edge, total

  allocate(work(0:5))
  do i = 0, 5
    work(i) = 2 * i + 1
  end do

  n = size(work)
  lo = lbound(work, 1)
  hi = ubound(work, 1)
  total = 0

  do i = lo, hi
    total = total + work(i)
  end do

  edge = work(lo) + work(hi)
  print *, n, lo, hi, edge, total
end program realworld_shape_guard
