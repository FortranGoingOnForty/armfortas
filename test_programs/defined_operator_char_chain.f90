! CHECK: ok
! IR_CHECK: call @afs_modproc_defined_operator_char_chain_m_join_op_char_char
! REPRO_CHECK: run
module defined_operator_char_chain_m
  implicit none

  interface operator(/)
    module procedure join_op_char_char
  end interface
contains
  function join_path(p1, p2) result(path)
    character(len=*), intent(in) :: p1
    character(len=*), intent(in) :: p2
    character(len=:), allocatable :: path

    if (len_trim(p1) == 0) then
      path = "/" // trim(p2)
    else
      path = trim(p1) // "/" // trim(p2)
    end if
  end function

  function join_op_char_char(p1, p2) result(path)
    character(len=*), intent(in) :: p1
    character(len=*), intent(in) :: p2
    character(len=:), allocatable :: path

    path = join_path(p1, p2)
  end function
end module

program defined_operator_char_chain
  use defined_operator_char_chain_m
  implicit none

  character(len=:), allocatable :: path

  path = ''/'home'/'Alice'/'.config'
  if (path /= '/home/Alice/.config') error stop 1
  print *, "ok"
end program
