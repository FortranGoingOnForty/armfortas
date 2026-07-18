! TOKENIZE's FIRST and LAST arrays may have different integer kinds.
! FLAGS: --std=f2023
! CHECK: first4-last8 T T
! CHECK: first8-last1 T T
! CHECK: first2-last4 T T
! CHECK: first1-last2 T T
program tokenize_mixed_integer_kinds
  implicit none
  integer(1), allocatable :: first1(:), last1(:)
  integer(2), allocatable :: first2(:), last2(:)
  integer(4), allocatable :: first4(:), last4(:)
  integer(8), allocatable :: first8(:), last8(:)

  call tokenize('a,b', ',', first4, last8)
  print *, 'first4-last8', all(first4 == [1_4, 3_4]), &
                            all(last8 == [1_8, 3_8])

  call tokenize('a,b', ',', first8, last1)
  print *, 'first8-last1', all(first8 == [1_8, 3_8]), &
                            all(last1 == [1_1, 3_1])

  call tokenize('a,b', ',', first2, last4)
  print *, 'first2-last4', all(first2 == [1_2, 3_2]), &
                            all(last4 == [1_4, 3_4])

  call tokenize('a,b', ',', first1, last2)
  print *, 'first1-last2', all(first1 == [1_1, 3_1]), &
                            all(last2 == [1_2, 3_2])
end program tokenize_mixed_integer_kinds
