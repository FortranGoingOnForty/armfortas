! Distinct pointer descriptors can designate the same target. Loop fusion
! must preserve the all-writes-before-reads ordering of adjacent loops.
program loop_fusion_pointer_alias
  implicit none
  integer, target :: storage(5)
  integer, pointer :: first(:), second(:)
  integer :: observed(4)

  storage = -1
  first => storage
  second => storage

  call run_loops(first, second, observed)

  print *, observed
contains
  subroutine run_loops(first, second, observed)
    integer, pointer, intent(inout) :: first(:), second(:)
    integer, intent(out) :: observed(4)
    integer :: i

    do i = 1, 4
      first(i) = i * 10
    end do
    do i = 1, 4
      observed(i) = second(i + 1)
    end do
  end subroutine
end program
! CHECK: 20 30 40 -1
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
