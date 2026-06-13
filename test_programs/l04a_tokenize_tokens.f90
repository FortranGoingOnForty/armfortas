! l04a: F2023 TOKENIZE Form 1 — CALL TOKENIZE(STRING, SET, TOKENS
! [, SEPARATOR]). TOKENS is an allocatable deferred-length character
! array the call allocates to the token count, each element padded to
! the longest token. SEPARATOR (optional) holds the separator that
! ended each token (count-1 single chars). Held identical by OPT_EQ.
! FLAGS: --std=f2023
! CHECK: n 3 len 3
! CHECK: tok [a  ]
! CHECK: tok [bb ]
! CHECK: tok [ccc]
! CHECK: sep [,] [;]
! CHECK: en 5 len 1
! CHECK: et [][x][][y][]
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program l04a_tokenize_tokens
  implicit none
  character(*), parameter :: s = "a,bb;ccc"
  character(*), parameter :: e = ",x,,y,"
  character(:), allocatable :: toks(:)
  character(:), allocatable :: seps(:)
  integer :: i, n, l

  call tokenize(s, ",;", toks, seps)
  n = size(toks)
  l = len(toks)
  write(*, '(A,1X,I0,1X,A,1X,I0)') 'n', n, 'len', l
  do i = 1, n
    write(*, '(3A)') 'tok [', toks(i), ']'
  end do
  write(*, '(5A)') 'sep [', seps(1), '] [', seps(2), ']'

  ! Empty-token case, padded to length 1.
  call tokenize(e, ",", toks)
  n = size(toks)
  l = len(toks)
  write(*, '(A,1X,I0,1X,A,1X,I0)') 'en', n, 'len', l
  ! Compact form, brackets around each trimmed token (empties show []).
  write(*, '(2A)') 'et ', trim(compact(toks, n))

  write(*, '(A)') 'ok'
contains
  function compact(arr, m) result(r)
    character(len=*), intent(in) :: arr(:)
    integer, intent(in) :: m
    character(len=64) :: r
    integer :: k
    r = ''
    do k = 1, m
      r = trim(r) // '[' // trim(arr(k)) // ']'
      if (k < m) r = trim(r) // ' '
    end do
  end function
end program l04a_tokenize_tokens
