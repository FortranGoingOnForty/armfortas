! Real-world-style DO CONCURRENT map that should keep the concurrent shape in
! raw IR and disappear after the small-loop exploitation path runs.
!
! CHECK: 3
! CHECK: 99
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_doconc_square
  implicit none
  integer :: i, a(10), b(10)

  do i = 1, 10
    a(i) = i
  end do

  do concurrent (i = 1:10)
    b(i) = a(i) * a(i) - 1
  end do

  print *, b(2)
  print *, b(10)
end program realworld_doconc_square
