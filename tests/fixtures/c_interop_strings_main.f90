! Differential Fortran side for l06 string interop. Drives C helpers
! (compiled with clang) and checks armfortas's view of shared memory
! against C's strlen/strcmp. Any mismatch is error stop with a distinct
! code so the harness can localize the failure.
program c_interop_strings_main
  use, intrinsic :: iso_c_binding
  implicit none

  interface
    function afs_test_hello() bind(c) result(p)
      import :: c_ptr
      type(c_ptr) :: p
    end function
    function afs_test_embedded() bind(c) result(p)
      import :: c_ptr
      type(c_ptr) :: p
    end function
    function afs_test_strlen(p) bind(c) result(n)
      import :: c_ptr, c_long
      type(c_ptr), value :: p
      integer(c_long) :: n
    end function
    function afs_test_streq(p, e) bind(c) result(r)
      import :: c_ptr, c_int
      type(c_ptr), value :: p
      type(c_ptr), value :: e
      integer(c_int) :: r
    end function
    function afs_test_rtbuf() bind(c) result(p)
      import :: c_ptr
      type(c_ptr) :: p
    end function
    function afs_test_rt_first() bind(c) result(c)
      import :: c_char
      character(kind=c_char) :: c
    end function
    function afs_test_ints() bind(c) result(p)
      import :: c_ptr
      type(c_ptr) :: p
    end function
  end interface

  character(len=:, kind=c_char), pointer :: s
  character(len=:, kind=c_char), allocatable, target :: built
  character(kind=c_char), allocatable, target :: expect(:)
  type(c_ptr) :: p
  integer, pointer :: iv(:)
  integer :: i

  ! ---- C_F_STRPOINTER, c_ptr form: C's "hello" seen by Fortran ----
  p = afs_test_hello()
  call c_f_strpointer(p, s, 5)
  if (len(s) /= 5) error stop 1
  if (s /= "hello") error stop 2
  ! Length agrees with C's strlen of the same bytes.
  if (int(afs_test_strlen(p)) /= len(s)) error stop 3

  ! ---- Embedded NUL: scan stops at the NUL even with NCHARS past it ----
  p = afs_test_embedded()
  call c_f_strpointer(p, s, 6)
  if (len(s) /= 2) error stop 4
  if (s /= "ab") error stop 5

  ! ---- F_C_STRING → C strcmp: trimmed content with terminating NUL ----
  built = f_c_string("abc   ")
  expect = [c_char_'a', c_char_'b', c_char_'c', c_null_char]
  if (afs_test_streq(c_loc(built), c_loc(expect)) /= 1) error stop 6
  ! ASIS keeps the blanks → not equal to "abc".
  built = f_c_string("abc   ", asis=.true.)
  if (afs_test_streq(c_loc(built), c_loc(expect)) /= 0) error stop 7

  ! ---- Round trip: C buffer → Fortran pointer → Fortran writes → C reads ----
  p = afs_test_rtbuf()
  call c_f_strpointer(p, s, 3)
  if (s /= "abc") error stop 8
  s(1:1) = 'Z'
  if (afs_test_rt_first() /= 'Z') error stop 9

  ! ---- C_F_POINTER with LOWER: view C's int[4] as iv(0:3) ----
  p = afs_test_ints()
  call c_f_pointer(p, iv, shape=[4], lower=[0])
  if (lbound(iv, 1) /= 0) error stop 10
  if (ubound(iv, 1) /= 3) error stop 11
  if (iv(0) /= 10) error stop 12
  if (iv(3) /= 40) error stop 13

  print '(A)', "c_interop_strings: all checks passed"
end program c_interop_strings_main
