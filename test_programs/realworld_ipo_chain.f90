! Real-world-style helper chain for intramodule IPO: dead arg trimming,
! constant specialization, and trivial return propagation.
!
! CHECK: 25
! CHECK: 258
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_ipo_chain
  implicit none
  integer :: i, data(6), total

  do i = 1, 6
    data(i) = i * 3
  end do

  call accumulate(data, total)
  print *, emit_value(data(2))
  print *, total

contains

  subroutine accumulate(values, total)
    integer, intent(in) :: values(6)
    integer, intent(out) :: total
    integer :: i

    total = 0
    do i = 1, 6
      total = total + emit_value(values(i))
    end do
  end subroutine accumulate

  integer function emit_value(x) result(r)
    integer, intent(in) :: x

    r = passthrough(mix_step(x, 4, 99))
  end function emit_value

  integer function passthrough(v) result(r)
    integer, intent(in) :: v

    r = v
  end function passthrough

  recursive integer function mix_step(x, scale, dead) result(r)
    integer, intent(in) :: x, scale, dead

    if (x < 0) then
      r = mix_step(-x, scale, dead)
      return
    end if

    r = x * scale + 1
  end function mix_step

end program realworld_ipo_chain
