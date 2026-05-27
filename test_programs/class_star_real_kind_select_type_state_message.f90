! CHECK: ok
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module class_star_real_kind_select_type_state_message_m
  use iso_fortran_env, only: sp => real32, dp => real64
  implicit none

  type :: state_type
    integer :: state = 0
    character(len=512) :: message = repeat(" ", 512)
  contains
    procedure :: parse
  end type state_type

  interface state_type
    module procedure new_state
  end interface state_type

contains
  pure type(state_type) function new_state(flag, a1, a2) result(new)
    integer, intent(in) :: flag
    class(*), optional, intent(in), dimension(..) :: a1, a2

    call new%parse(flag, a1, a2)
  end function new_state

  pure subroutine parse(this, flag, a1, a2)
    class(state_type), intent(inout) :: this
    integer, intent(in) :: flag
    class(*), optional, intent(in), dimension(..) :: a1, a2

    this%state = flag
    this%message = ""
    call appendr(this%message, a1)
    call appendr(this%message, a2)
  end subroutine parse

  pure subroutine appendr(msg, a)
    character(len=*), intent(inout) :: msg
    class(*), optional, intent(in), dimension(..) :: a

    if (present(a)) then
      select rank (v => a)
      rank (0)
        call append(msg, v)
      rank default
        msg = trim(msg)//" <rank>"
      end select
    end if
  end subroutine appendr

  pure subroutine append(msg, a)
    character(len=*), intent(inout) :: msg
    class(*), intent(in) :: a
    character(len=512) :: buffer

    select type (aa => a)
    type is (character(len=*))
      msg = trim(msg)//aa
    type is (real(sp))
      write(buffer, "(es15.8e2)") aa
      msg = trim(msg)//trim(adjustl(buffer))
    type is (real(kind=dp))
      write(buffer, "(es24.16e3)") aa
      msg = trim(msg)//trim(adjustl(buffer))
    class default
      msg = trim(msg)//" <type>"
    end select
  end subroutine append
end module class_star_real_kind_select_type_state_message_m

program class_star_real_kind_select_type_state_message
  use iso_fortran_env, only: sp => real32, dp => real64
  use class_star_real_kind_select_type_state_message_m, only: state_type
  implicit none

  type(state_type) :: state

  state = state_type(0, "r32=", 1.0_sp)
  if (trim(state%message) /= "r32=1.00000000E+00") error stop 1

  state = state_type(0, "r64=", 1.0_dp)
  if (trim(state%message) /= "r64=1.0000000000000000E+000") error stop 2

  write(*, "(a)") "ok"
end program class_star_real_kind_select_type_state_message
