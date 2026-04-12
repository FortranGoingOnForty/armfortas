! BLOCK construct variable scoping: a declaration inside a BLOCK
! shadows the outer variable for the body, then the outer value
! is restored after END BLOCK.
program block_scope_shadow
  implicit none
  integer :: x, n
  x = 100
  n = 1
  block
    integer :: x
    x = 6
    print *, x
  end block
  print *, x
  block
    integer :: n
    n = 2
    block
      integer :: n
      n = 3
      print *, n
    end block
    print *, n
  end block
  print *, n
end program
! CHECK: 6
! CHECK: 100
! CHECK: 3
! CHECK: 2
! CHECK: 1
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
