! Real-world-style assumed-shape SIZE query canary.
! CHECK: 6
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_assumed_shape_size
  implicit none
  integer :: data(6), total, i

  do i = 1, 6
    data(i) = i
  end do

  call count_values(data, total)
  print *, total

contains

  subroutine count_values(values, total)
    integer, intent(in) :: values(:)
    integer, intent(out) :: total

    total = size(values)
  end subroutine count_values

end program realworld_assumed_shape_size
