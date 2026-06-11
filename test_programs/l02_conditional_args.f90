! l02: F2023 conditional actual arguments — the conditional selects an
! argument ASSOCIATION (INTENT(OUT) writes land in the chosen actual,
! never a temporary), and .NIL. passes the absent association for an
! OPTIONAL dummy, observed via PRESENT().
! FLAGS: --std=f2023
! CHECK: 42 99
! CHECK: present 7
! CHECK: absent
! CHECK: present 5
! CHECK: absent
program l02_conditional_args
  implicit none
  integer :: a, c
  logical :: yes, no
  a = 10
  c = 99
  yes = .true.
  no = .false.
  call setit((a > 5 ? a : c), 42)
  print *, a, c
  call maybe((.true. ? 7 : .nil.))
  call maybe((.false. ? 7 : .nil.))
  call maybe((yes ? 5 : .nil.))
  call maybe((no ? 5 : .nil.))
contains
  subroutine setit(x, v)
    integer, intent(out) :: x
    integer, intent(in) :: v
    x = v
  end subroutine setit
  subroutine maybe(o)
    integer, intent(in), optional :: o
    if (present(o)) then
      print *, 'present', o
    else
      print *, 'absent'
    end if
  end subroutine maybe
end program l02_conditional_args
