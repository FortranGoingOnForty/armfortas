! Module-level character(len=:), allocatable string set in a module
! procedure and read back in the main program.  Previously the
! module global was emitted as the element type size (1-8 bytes)
! instead of a 32-byte StringDescriptor, so the length field was
! missing and print produced blank output.
module msg_mod
  implicit none
  character(len=:), allocatable :: msg
contains
  subroutine set_msg(s)
    character(len=*), intent(in) :: s
    msg = s
  end subroutine
end module
program module_deferred_string
  use msg_mod
  call set_msg("hello")
  print *, msg
  call set_msg("world!")
  print *, msg
  print *, len(msg)
end program
! CHECK: hello
! CHECK: world!
! CHECK: 6
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
