! CHECK: ok
! IR_CHECK: call @afs_modproc_proc_dummy_iface_scope_m_new_string(
! IR_CHECK: call %
! REPRO_CHECK: run
module proc_dummy_iface_scope_m
  implicit none

  type :: string_type
    private
    character(len=:), allocatable :: raw
  end type string_type

  type :: error_type
    integer :: code = 0
  end type error_type

  interface string_type
    module procedure new_string
    module procedure new_integer
  end interface string_type

  abstract interface
    subroutine check1_interface(error, str1, chr1)
      import :: error_type, string_type
      type(error_type), allocatable, intent(out) :: error
      type(string_type), intent(in) :: str1
      character(len=*), intent(in) :: chr1
    end subroutine check1_interface

    subroutine check2_interface(error, str1, chr1, str2, chr2)
      import :: error_type, string_type
      type(error_type), allocatable, intent(out) :: error
      type(string_type), intent(in) :: str1, str2
      character(len=*), intent(in) :: chr1, chr2
    end subroutine check2_interface
  end interface

contains

  function new_string(text) result(out)
    character(len=*), intent(in), optional :: text
    type(string_type) :: out
    if (present(text)) then
      out%raw = text
    else
      out%raw = ''
    end if
  end function new_string

  function new_integer(value) result(out)
    integer, intent(in) :: value
    type(string_type) :: out
    if (value == 0) then
      out%raw = ''
    else
      out%raw = 'x'
    end if
  end function new_integer

  subroutine constructor_check1(error, chr1, checker)
    type(error_type), allocatable, intent(out) :: error
    character(len=*), intent(in) :: chr1
    procedure(check1_interface) :: checker
    call checker(error, string_type(chr1), chr1)
  end subroutine constructor_check1

  subroutine constructor_check2(error, chr1, chr2, checker)
    type(error_type), allocatable, intent(out) :: error
    character(len=*), intent(in) :: chr1, chr2
    procedure(check2_interface) :: checker
    call checker(error, string_type(chr1), chr1, string_type(chr2), chr2)
  end subroutine constructor_check2

  subroutine check_pair(error, str1, chr1, str2, chr2)
    type(error_type), allocatable, intent(out) :: error
    type(string_type), intent(in) :: str1, str2
    character(len=*), intent(in) :: chr1, chr2

    if (.not. allocated(str1%raw)) allocate(error)
    if (.not. allocated(str2%raw)) allocate(error)
    if (str1%raw /= 'abc') allocate(error)
    if (str2%raw /= 'de') allocate(error)
    if (len(chr1) /= 3) allocate(error)
    if (len(chr2) /= 2) allocate(error)
    if (chr1 /= 'abc') allocate(error)
    if (chr2 /= 'de') allocate(error)
  end subroutine check_pair

end module proc_dummy_iface_scope_m

program p
  use proc_dummy_iface_scope_m
  implicit none
  type(error_type), allocatable :: error

  call constructor_check2(error, 'abc', 'de', check_pair)
  if (allocated(error)) error stop 1
  print *, 'ok'
end program p
