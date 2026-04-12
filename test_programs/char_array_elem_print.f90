! Print individual elements of a character array.
! Previously is_char required dims.is_empty() which excluded
! array element access; the print path fell through to the
! non-character path and printed blank output.
program char_array_elem_print
  implicit none
  character(len=5) :: words(3)
  words(1) = "hello"
  words(2) = "world"
  words(3) = "test!"
  print *, words(1)
  print *, words(2)
  print *, words(3)
end program
! CHECK: hello
! CHECK: world
! CHECK: test!
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
