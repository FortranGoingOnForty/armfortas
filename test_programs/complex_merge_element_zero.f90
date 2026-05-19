! CHECK: ok
! IR_CHECK: select
! IR_CHECK: ptr<[f32 x 2]>
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|asm|obj|repro
program complex_merge_element_zero
  implicit none
  complex, parameter :: zero = 0.0
  complex :: a(3,2), r(3,2)
  integer :: i, j

  a = (2.0, 3.0)
  r = (9.0, 9.0)
  forall(i=1:3,j=2:2) r(i,j) = merge(a(i,j), zero, i <= j)
  if (r(1,2) /= (2.0, 3.0)) error stop 1
  if (r(2,2) /= (2.0, 3.0)) error stop 2
  if (r(3,2) /= (0.0, 0.0)) error stop 3
  print *, "ok"
end program
