! l02a item 5: conditional expression in a character LEN spec (F2023).
! `character(len=(n > 5 ? n : 5))` gives length n when n>5, else 5. The
! parser used to choke on `?` because parse_len_spec consumed the len `(`
! before the conditional probe could fire.
! FLAGS: --std=f2023
program l02a_len_conditional
  implicit none
  print '(I0)', len(f(3))
  ! CHECK: 5
  print '(I0)', len(f(8))
  ! CHECK: 8
  ! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|exit
contains
  function f(n) result(str)
    integer, value :: n
    character(len=(n > 5 ? n : 5)) :: str
    str = ""
  end function f
end program l02a_len_conditional
