! Real-world-style postprocess stage combining whole-array ELEMENTAL mapping
! with a clean DO CONCURRENT array combine.
!
! CHECK: 4
! CHECK: 16
! CHECK: 40
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_elemental_stage
  implicit none
  integer :: i, raw(8), bias(8), mapped(8), out(8)

  do i = 1, 8
    raw(i) = i
    bias(i) = i * 2
  end do

  mapped = mix(raw, bias)

  do concurrent (i = 1:8)
    out(i) = mapped(i) + raw(i)
  end do

  print *, mapped(1)
  print *, mapped(4)
  print *, out(8)

contains

  elemental function mix(x, y) result(r)
    integer, intent(in) :: x, y
    integer :: r

    r = x * 2 + y
  end function mix

end program realworld_elemental_stage
