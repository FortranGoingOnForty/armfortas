! Real-world-style explicit DO map that should redirect to the bulk runtime
! kernel only at O3/Ofast.
!
! CHECK: 10
! CHECK: 67
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_vector_stage
  implicit none
  integer :: i, base(32), delta(32), out(32)

  do i = 1, 32
    base(i) = i
    delta(i) = i * 2 + 1
  end do

  do i = 1, 32
    out(i) = base(i) + delta(i)
  end do

  print *, out(3)
  print *, out(22)
end program realworld_vector_stage
