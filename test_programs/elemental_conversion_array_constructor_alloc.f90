! CHECK: 21
! IR_CHECK: elemental_call_check
! IR_CHECK: call @afs_assign_allocatable
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program elemental_conversion_array_constructor_alloc
  implicit none
  integer, parameter :: sp = kind(1.0)
  real(sp), allocatable :: x(:)

  x = real([1, 2, 3, 4, 5, 6], kind=sp)
  print *, int(sum(x))
end program elemental_conversion_array_constructor_alloc
