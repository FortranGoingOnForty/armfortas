! TRIM / LEN_TRIM on character(len=*) and deferred-length strings.
! Previously the TRIM fast path used char_addr_and_len (compile-time
! length only); AssumedLen and Deferred fell through to an undefined
! external _trim symbol.
program trim_assumed_len
  implicit none
  character(len=:), allocatable :: s
  call show("  hello  ")
  call show("abc   ")
  s = "  world  "
  print *, trim(s)
contains
  subroutine show(arg)
    character(len=*), intent(in) :: arg
    print *, trim(arg)
    print *, len_trim(arg)
  end subroutine show
end program
! CHECK: hello
! CHECK: 7
! CHECK: abc
! CHECK: 3
! CHECK: world
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
