! CHECK: ok
! IR_CHECK: __prog_character_transfer_array_roundtrip
! REPRO_CHECK: run
program character_transfer_array_roundtrip
  implicit none
  character(len=*), parameter :: test = "abcd"
  character(len=1) :: fixed(4)
  character(len=4) :: roundtrip
  character(len=1), allocatable :: dyn(:)
  character(len=:), allocatable :: dyn_roundtrip
  character(len=:), allocatable :: sliced

  fixed = transfer(test, fixed)
  if (iachar(fixed(1)) /= iachar("a")) error stop 1
  if (iachar(fixed(2)) /= iachar("b")) error stop 2
  if (iachar(fixed(3)) /= iachar("c")) error stop 3
  if (iachar(fixed(4)) /= iachar("d")) error stop 4

  roundtrip = transfer(fixed, roundtrip)
  if (roundtrip /= test) error stop 5

  dyn = fixed
  dyn = dyn(1:4:1)
  dyn_roundtrip = transfer(dyn, roundtrip)
  if (dyn_roundtrip /= test) error stop 6

  sliced = reference_slice( &
      "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789", first=-2)
  if (sliced /= "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789") error stop 7
  sliced = reference_slice( &
      "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789", first=2)
  if (sliced /= "bcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789") error stop 8
  sliced = reference_slice( &
      "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789", stride=-61)
  if (sliced /= "9a") error stop 9

  print *, "ok"

contains
  pure integer function optval(value, default) result(out)
    integer, intent(in), optional :: value
    integer, intent(in) :: default

    if (present(value)) then
      out = value
    else
      out = default
    end if
  end function optval

  pure function reference_slice(string, first, stride) result(sliced_string)
    character(len=*), intent(in) :: string
    integer, intent(in), optional :: first
    integer, intent(in), optional :: stride
    character(len=:), allocatable :: sliced_string
    character(len=1), allocatable :: carray(:)

    integer :: first_, last_, stride_

    stride_ = 1
    if (present(stride)) then
      stride_ = merge(stride_, stride, stride == 0)
    end if

    if (stride_ < 0) then
      last_ = 1
      first_ = min(max(optval(first, len(string)), 0), len(string))
    else
      first_ = min(max(optval(first, 1), 1), len(string)+1)
      last_ = len(string)
    end if

    carray = string_to_carray(string)
    carray = carray(first_:last_:stride_)
    sliced_string = carray_to_string(carray)
  end function reference_slice

  pure function string_to_carray(string) result(carray)
    character(len=*), intent(in) :: string
    character(len=1) :: carray(len(string))

    carray = transfer(string, carray)
  end function string_to_carray

  pure function carray_to_string(carray) result(string)
    character(len=1), intent(in) :: carray(:)
    character(len=size(carray)) :: string

    string = transfer(carray, string)
  end function carray_to_string
end program
