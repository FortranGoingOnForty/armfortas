! CHECK: ok
! IR_CHECK: call @afs_copy_array_data
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program array_self_reverse_section_assignment
  implicit none
  real :: fixed(2)
  real, allocatable :: dyn(:)

  fixed = [0.0, 8.0]
  fixed = fixed(2:1:-1)
  if (abs(fixed(1) - 8.0) > 1.0e-6) error stop 1
  if (abs(fixed(2)) > 1.0e-6) error stop 2

  allocate(dyn(2))
  dyn = [0.0, 8.0]
  dyn = dyn(2:1:-1)
  if (abs(dyn(1) - 8.0) > 1.0e-6) error stop 3
  if (abs(dyn(2)) > 1.0e-6) error stop 4

  write(*, "(a)") "ok"
end program array_self_reverse_section_assignment
