! CHECK: ok
! IR_CHECK: call @afs_modproc_class_star_message_m_appendr
! IR_CHECK: call @afs_modproc_class_star_message_m_append
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module class_star_message_m
  implicit none
  integer, parameter :: fs_error = -5

  type :: state_type
    integer :: state = 0
    character(len=512) :: message = repeat(" ", 512)
  end type state_type

  interface state_type
    module procedure new_state
  end interface state_type

contains
  pure function fs_error_code(code, a1, a2) result(state)
    type(state_type) :: state
    integer, intent(in) :: code
    class(*), intent(in), optional, dimension(..) :: a1, a2
    character(32) :: code_msg

    write(code_msg, "('code - ', i0, ',')") code
    state = state_type(fs_error, code_msg, a1, a2)
  end function fs_error_code

  pure type(state_type) function new_state(flag, a1, a2, a3) result(state)
    integer, intent(in) :: flag
    class(*), intent(in), optional, dimension(..) :: a1, a2, a3

    state%state = flag
    state%message = ""
    call appendr(state%message, a1)
    call appendr(state%message, a2)
    call appendr(state%message, a3)
  end function new_state

  pure subroutine appendr(msg, a)
    character(len=*), intent(inout) :: msg
    class(*), intent(in), optional :: a(..)

    if (present(a)) then
      select rank (v => a)
      rank (0)
        call append(msg, v)
      rank default
        msg = trim(msg) // " <BADRANK>"
      end select
    end if
  end subroutine appendr

  pure subroutine append(msg, a)
    character(len=*), intent(inout) :: msg
    class(*), intent(in) :: a
    character(len=2) :: sep
    integer :: ls

    sep = "  "
    ls = merge(1, 0, len_trim(msg) > 0)

    select type (aa => a)
    type is (character(len=*))
      msg = trim(msg) // sep(:ls) // aa
    type is (integer)
      write(msg, "(a,i0)") trim(msg) // sep(:ls), aa
    class default
      msg = trim(msg) // " <BADTYPE>"
    end select
  end subroutine append
end module class_star_message_m

program class_star_character_select_type_message
  use class_star_message_m
  implicit none

  type(state_type) :: s
  character(:), allocatable :: expected

  expected = "code - 10, Cannot create File temp.txt - File already exists"
  s = fs_error_code(10, "Cannot create File temp.txt -", "File already exists")
  if (s%state /= fs_error) error stop 1
  if (trim(s%message) /= expected) error stop 2

  write(*, "(a)") "ok"
end program class_star_character_select_type_message
