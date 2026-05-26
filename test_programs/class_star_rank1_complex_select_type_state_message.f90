! CHECK: ok
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro

module class_star_rank1_complex_select_type_state_message_m
  use iso_fortran_env, only: real32, real64
  implicit none

contains
  subroutine append_rank(msg, a)
    character(len=*), intent(inout) :: msg
    class(*), optional, intent(in), dimension(..) :: a

    if (present(a)) then
      select rank (v => a)
      rank (1)
        call append_vector(msg, v)
      rank default
        msg = trim(msg)//" <rank>"
      end select
    end if
  end subroutine append_rank

  subroutine append_vector(msg, a)
    character(len=*), intent(inout) :: msg
    class(*), intent(in) :: a(:)
    character(len=512) :: buffer, buffer2

    msg = trim(msg)//" ["
    select type (aa => a)
    type is (real(real32))
      write(buffer, "(es15.8e2)") aa(1)
      msg = trim(msg)//trim(adjustl(buffer))
    type is (real(real64))
      write(buffer, "(es24.16e3)") aa(1)
      msg = trim(msg)//trim(adjustl(buffer))
    type is (complex(real32))
      write(buffer, "(es15.8e2)") aa(1)%re
      write(buffer2, "(es15.8e2)") aa(1)%im
      msg = trim(msg)//"("//trim(adjustl(buffer))//","//trim(adjustl(buffer2))//")"
    class default
      msg = trim(msg)//" <type>"
    end select
    msg = trim(msg)//"]"
  end subroutine append_vector
end module class_star_rank1_complex_select_type_state_message_m

program class_star_rank1_complex_select_type_state_message
  use iso_fortran_env, only: real32
  use class_star_rank1_complex_select_type_state_message_m, only: append_rank
  implicit none

  character(len=512) :: msg

  msg = "v="
  call append_rank(msg, [(1.0_real32, 0.0_real32), (0.0_real32, 1.0_real32)])
  if (index(msg, "<rank>") /= 0) error stop 1
  if (index(msg, "<type>") /= 0) error stop 2
  if (index(msg, "1.00000000E+00") == 0) error stop 3
  if (index(msg, "0.00000000E+00") == 0) error stop 4
  print *, "ok"
end program class_star_rank1_complex_select_type_state_message
