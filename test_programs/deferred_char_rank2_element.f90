! Audit C10: reading a rank>=2 character array element returned length 0.
! `g(1,1) == 'aa'` was false and `g(1,1)` printed empty even though len() was
! right, because the character-array-element string read (lower_string_expr_full
! FunctionCall arm) was gated on args.len()==1 — a rank-2 element g(i,j) fell
! through to the generic call path, which produced the correct data pointer but
! a length of 0. char_array_element_ptr_and_len / compute_flat_elem_offset
! already fold every subscript into the flat offset, so lifting the
! single-subscript gate (accept any all-scalar subscript list) fixes every rank.
program deferred_char_rank2_element
  character(:), allocatable :: g(:,:)
  character(:), allocatable :: t(:,:,:)
  character(4), allocatable :: fx(:,:)
  character(3) :: stat(2,2)

  allocate(character(2) :: g(2,2))
  allocate(character(2) :: t(2,2,2))
  allocate(fx(2,2))

  g(1,1) = 'aa'; g(1,2) = 'bb'; g(2,1) = 'cc'; g(2,2) = 'dd'
  t = 'ab'; t(2,2,2) = 'XY'
  fx(1,2) = 'abcd'
  stat(2,1) = 'foo'

  ! rank-2 deferred element: compare, read, length
  print '(A,L2)', 'e11', g(1,1) == 'aa'
  ! CHECK: e11 T
  print '(A,L2)', 'e22', g(2,2) == 'dd'
  ! CHECK: e22 T
  print '(A,A,A)', '[', g(2,1), ']'
  ! CHECK: [cc]
  print '(A,I0)', 'len=', len(g(1,1))
  ! CHECK: len=2
  ! substring of a rank-2 deferred element
  print '(A,A)', 's=', g(1,2)(2:2)
  ! CHECK: s=b
  ! rank-3 deferred element
  print '(A,L2)', 'e3', t(2,2,2) == 'XY'
  ! CHECK: e3 T
  ! fixed-length rank-2 allocatable + static elements still read correctly
  print '(A,A)', 'fx=', fx(1,2)
  ! CHECK: fx=abcd
  print '(A,A)', 'st=', stat(2,1)
  ! CHECK: st=foo
end program
